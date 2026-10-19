// bfs.rs — forward BFS with step-action space and deterministic QB tracking.
//
// Core idea: instead of tracking a *set* of possible current positions per QB,
// we track a deterministic function from start positions to current positions.
// Each BFS node carries a QState — an array over all QB start slots, each
// holding the current FlatIdx for that start, or INVALID (wall-invalidated).
//
// A "valid assignment" is a choice of one slot per QB such that:
//   - every chosen slot has cur != INVALID  (not wall-invalidated), and
//   - no two chosen slots share the same cur value  (no collision).
//
// SHORTCUT TRACKING
// -----------------
// We accumulate a list of "shortcut qstates" — one per node that reached exit
// early. An assignment A is "covered" (shortcut) if it is a valid assignment
// of some shortcut qstate. Each BFS node carries a "witness assignment" — a
// valid assignment of its qstate that is not covered by any shortcut. A node
// is pruned when no such witness exists.
//
// At the end, for each slot i, we ask: is there any valid assignment containing
// slot i that is not covered by any shortcut? If no, slot i is ruled out.
//
// OUTPUT INTERFACE
// ----------------
//   Unreachable   — no assignment reaches exit in exactly target_steps.
//   AllShortcut   — every assignment is covered by shortcuts.
//   SomeClean { ruled_out } — at least one assignment is not covered.
//       ruled_out: (original QB index, start_pos) pairs to remove from the board.

use rustc_hash::{FxHashMap, FxHashSet};
use crate::search_log::{print, println};
use smallvec::{SmallVec};

use crate::board::{QuantumBoard, MAX_TILES};
use crate::deadlock::{effective_tile, BoxState};
use crate::qstate::{
    any_slots_at, find_uncovered_witness, has_valid_assignment,
    slot_has_uncovered_assignment, slot_mask_at, Assignment, AssignmentPool, QState, Shortcut,
    ShortcutConstraints, StartSlot, INVALID_SLOT, FREE_SLOT, MAX_STARTS,
};
use crate::bfsrelaxed::{RelaxedCache,RelaxedResult, relaxed_solve};
use crate::types::{Dir, FlatIdx, QBlockId, Tile};
use crate::shortcut_paths::{PathTracker, ShortcutPaths};

// ── VisitedTracker trait ───────────────────────────────────────────────────────
//
// Abstract interface for visited state tracking. Allows both simple FxHashSet
// and map-based tracking (e.g., with parent pointers).

pub trait VisitedTracker {
    fn contains(&self, key: &StateKey) -> bool;
    /// Read-only: rejected witnesses must never reserve a queued arrival.
    fn redundant(&self, key: &StateKey, _steps: u32) -> bool { self.contains(key) }
    fn insert(&mut self, key: &StateKey, steps: u32) -> bool;
    fn len(&self) -> usize;
    fn dominated(&mut self, _node: &BfsNode) -> bool { false }
    fn record_path(&mut self, _child: &BfsNode, _parent: &StateKey, _walk_end: FlatIdx,
        _final_move: Option<Dir>, _dists: &[u8; MAX_TILES], _width: u16) {}
    fn solution_path(&self, _key: &StateKey) -> Option<Vec<Dir>> { None }
    /// Return false if this arrival is dominated by an already queued/settled one.
    fn record_parent(&mut self, child: &BfsNode, parent_key: &StateKey, _slots: &[StartSlot]) -> bool {
        self.record_parent_key(child, node_key(child), parent_key)
    }
    fn record_parent_key(&mut self, _child: &BfsNode, _key: StateKey, _parent_key: &StateKey) -> bool { true }
}

pub struct SimpleVisited {
    // u32::MAX marks a settled state; other values are best queued costs.
    costs: FxHashMap<StateKey, u32>,
    settled: usize,
    #[cfg(feature = "indexed-dominance")]
    dominance: crate::dominance::DominanceIndex,
    paths: Option<PathTracker>,
}

impl SimpleVisited {
    pub fn new() -> Self {
        Self {
            costs: FxHashMap::default(), settled: 0, paths: None,
            #[cfg(feature = "indexed-dominance")]
            dominance: Default::default(),
        }
    }
}

impl VisitedTracker for SimpleVisited {
    fn record_path(&mut self, child: &BfsNode, parent: &StateKey, walk_end: FlatIdx,
        final_move: Option<Dir>, dists: &[u8; MAX_TILES], width: u16) {
        if let Some(paths) = &mut self.paths {
            paths.record(node_key(child), parent, walk_end, final_move, dists, width);
        }
    }

    fn solution_path(&self, key: &StateKey) -> Option<Vec<Dir>> {
        self.paths.as_ref()?.path(key)
    }

    #[cfg(feature = "indexed-dominance")]
    fn dominated(&mut self, node: &BfsNode) -> bool {
        self.dominance.check_and_insert(node)
    }

    fn contains(&self, key: &StateKey) -> bool {
        self.costs.get(key) == Some(&u32::MAX)
    }

    fn redundant(&self, key: &StateKey, steps: u32) -> bool {
        self.costs.get(key).is_some_and(|&cost| cost == u32::MAX || cost <= steps)
    }

    fn insert(&mut self, key: &StateKey, steps: u32) -> bool {
        let cost = self.costs.entry(key.clone()).or_insert(steps);
        if *cost == u32::MAX || steps > *cost { return false; }
        *cost = u32::MAX;
        self.settled += 1;
        true
    }

    fn len(&self) -> usize {
        self.settled
    }

    fn record_parent_key(&mut self, child: &BfsNode, key: StateKey, _parent_key: &StateKey) -> bool {
        match self.costs.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let cost = *entry.get();
                if cost == u32::MAX || cost <= child.steps { return false; }
                entry.insert(child.steps);
            }
            std::collections::hash_map::Entry::Vacant(entry) => { entry.insert(child.steps); }
        }
        true
    }
}

// ── BfsNode ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct BfsNode {
    pub player: FlatIdx,
    pub steps: u32,
    pub qstate: QState,
    pub box_state: BoxState,
    /// Index into the shared assignment pool for a valid assignment of `qstate`
    /// not covered by any known shortcut. `None` means not assigned yet; a
    /// lazy refresh will fill it. While the node is in the queue, we guarantee
    /// that this resolves to Some(index) (we prune nodes without a witness).
    pub witness_idx: Option<usize>,
    //history: SmallVec<[Dir; 16]>,
}

// ── SearchContext ─────────────────────────────────────────────────────────────

pub struct SearchContext {
    pub q_occupancy: u128,
    pub box_occupancy: u128,
    pub covered_mask: u128,
    pub static_walls: u128,
    pub static_boxes: u128,
    pub box_origin_groups: [u128; 16],
    pub static_inner_mask: u128,
    /// For each direction, a bitmask of tiles that can be pushed in that direction.
    /// A tile is pushable in a direction if:
    ///   (a) it holds a concrete box whose push_mask includes the direction, OR
    ///   (b) it holds at least one valid QB slot whose push_mask includes the direction.
    pub pushable: [u128; 4],
    pub relaxed_cache: RelaxedCache,
    /// Shortest exit distances with all blocks removed, but fixed walls retained.
    pub exit_distances: [u8; MAX_TILES],
    #[cfg(feature = "walking-dominance")]
    pub walks: crate::walking_dominance::WalkingDominance,
}

pub fn wall_distances(board: &QuantumBoard, exit: FlatIdx) -> [u8; MAX_TILES] {
    let mut distances = [u8::MAX; MAX_TILES];
    let mut queue = [0; MAX_TILES];
    distances[exit as usize] = 0;
    queue[0] = exit;
    let (mut head, mut tail) = (0, 1);
    while head < tail {
        let pos = queue[head];
        head += 1;
        let (x, y) = board.coords(pos);
        for dir in Dir::all() {
            if let Some((nx, ny)) = dir.apply(x, y) {
                if !board.in_inner(nx, ny) { continue; }
                let next = board.flat(nx, ny);
                if board.get_flat(next) != Tile::Wall && distances[next as usize] == u8::MAX {
                    distances[next as usize] = distances[pos as usize] + 1;
                    queue[tail] = next;
                    tail += 1;
                }
            }
        }
    }
    distances
}

// ── State identity ───────────────────────────────────────────────────────────────

/// Exact, compact state identity. Hash-table collisions must still compare data.
/// Slots, push masks, walls and exit are fixed for the owning forward solve.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StateKey(SmallVec<[u8; 24]>);

pub fn node_key(node: &BfsNode) -> StateKey {
    state_key(node.player, &node.qstate, &node.box_state)
}
#[inline(never)] // Mark this explicitely, as it's a known hotspot we want to measure
pub fn state_key(player: FlatIdx, qstate: &QState, box_state: &BoxState) -> StateKey {
    let mut data = SmallVec::with_capacity(3 + qstate.len + 2 * box_state.moved.len());
    data.extend_from_slice(&player.to_le_bytes());
    data.push(qstate.len as u8);
    data.extend_from_slice(&qstate.cur[..qstate.len]);
    for &(origin, current) in &box_state.moved {
        // Board positions and FREE_SLOT fit in a byte; INVALID is QB-only.
        debug_assert!(origin <= u8::MAX as FlatIdx && current <= u8::MAX as FlatIdx);
        data.push(origin as u8);
        data.push(current as u8);
    }
    StateKey(data)
}
// ── BfsResult ─────────────────────────────────────────────────────────────────

pub enum BfsResult {
    Unreachable,
    AllShortcut,
    SomeClean {
        ruled_out: Vec<(usize, FlatIdx)>,
        shortcuts: Vec<Shortcut>,
        /// A clean assignment exists with the exit initially uncovered.
        /// False does not prune backward exploration of this intermediate state.
        publishable: bool,
    },
    Indeterminate {
        ruled_out: Vec<(usize, FlatIdx)>,
        shortcuts: Vec<Shortcut>,
    },
}

fn add_shortcut_qstate(
    shortcut_constraints: &mut Vec<ShortcutConstraints>,
    shortcut_set: &mut FxHashSet<QState>,
    qstate: QState,
    slots: &[StartSlot],
) -> bool {
    // 1. Fast identical-state check via the global hash set
    if shortcut_set.contains(&qstate) {
        return false;
    }

    let new_c = ShortcutConstraints::from_qstate(&qstate, slots);

    // Helper closure to determine if shortcut `left` subsumes shortcut `right`.
    // Left subsumes Right if Left's constraints are a subset of Right's constraints.
    let subsumes = |left: &ShortcutConstraints, right: &ShortcutConstraints| -> bool {
        // Unary subset check: Are all invalid slots in `left` also invalid in `right`?
        if (left.invalid_slots_mask & !right.invalid_slots_mask) != 0 {
            return false;
        }

        // Binary subset check: Is every collision pair in `left` also present in `right`?
        // Because collision_pairs is a small, sparse collection, this loop is highly localized.
        for &(la, lb) in &left.collision_pairs {
            let mut found_in_right = false;
            for &(ra, rb) in &right.collision_pairs {
                if (la == ra && lb == rb) || (la == rb && lb == ra) {
                    found_in_right = true;
                    break;
                }
            }
            if !found_in_right {
                return false;
            }
        }

        true
    };

    // 2. Bidirectional Subsumption Check
    // Check if any existing shortcut already subsumes our new candidate shortcut.
    for existing in shortcut_constraints.iter() {
        if subsumes(existing, &new_c) {
            // The existing shortcut is more general/powerful than the new one. 
            // The new shortcut provides no extra pruning capabilities; drop it.
            return false;
        }
    }

    // Since it wasn't subsumed, we add the base qstate to our deduplication set.
    shortcut_set.insert(qstate);

    // Prune the existing list: Remove any old shortcuts that are subsumed by our new shortcut.
    // retain() acts in-place and keeps the elements for which the closure returns true.
    shortcut_constraints.retain(|existing| {
        // If the new shortcut subsumes the existing one, return false to drop the existing one.
        !subsumes(&new_c, existing)
    });

    // 3. Append and Restore the Fail-First Ordering Strategy
    shortcut_constraints.push(new_c);
    shortcut_constraints.sort_unstable_by_key(|c| c.collision_pairs.len());
    true
}

/// Try to refresh a node's witness using a shared pool first, then fall back
/// to a full search. Returns an index into `pool` for a valid, uncovered
/// assignment, or `None` if none exists.
pub fn refresh_witness_in_pool(
    witness_idx: Option<usize>,
    pool: &mut AssignmentPool,
    qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
) -> Option<usize> {
    #[cfg(feature = "profile-search")]
    let _timer = crate::search_profile::Timer::start(1);
    // Fast-path: existing witness index still valid
    if let Some(idx) = witness_idx {
        if idx < pool.len() {
            let a = &pool[idx];
            if crate::qstate::assignment_valid_in(a, qstate) && !crate::qstate::assignment_covered(a, shortcuts) {
                return Some(idx);
            }
        }
    }

    // A failed carried witness often means this state has no witness at all.
    // Search its small domains directly instead of rescanning the historical pool.
    if let Some(a) = find_uncovered_witness(qstate, slots, num_qbs, shortcuts) {
        return Some(pool.intern(a));
    }
    None
}

// ── BfsCallbacks trait ────────────────────────────────────────────────────────
//
// Decouples `process_bfs_node` from the shortcut/result bookkeeping that is
// specific to the forward BFS.  `RelaxedBfsCallbacks` (in bfsrelaxed.rs)
// provides no-op stubs so that `relaxed_solve` can call `process_bfs_node`
// without carrying any shortcut state.

pub trait BfsCallbacks {
    fn on_shortcut_path(&mut self, _path: Vec<Dir>) {}
    /// Called when a node reaches the exit strictly before `target_steps`.
    /// The impl is responsible for recording the shortcut qstate if desired.
    fn on_shortcut_found(&mut self, qstate: &QState, slots: &[StartSlot], verbose: u8);

    /// Called when a node reaches the exit at exactly `target_steps`.
    /// `shortcut_constraints` is passed in (rather than accessed through
    /// `self`) to avoid a double-borrow when the caller holds both the
    /// callbacks and the constraints.
    fn on_clean_exit(
        &mut self,
        node: &BfsNode,
        pool: &[Assignment],
        slots: &[StartSlot],
        num_qbs: usize,
        shortcut_constraints: &[ShortcutConstraints],
    );

    /// Returns `true` if the zero-QB fast-path should prune this node because
    /// every assignment is already a shortcut and no clean exit has been seen.
    /// The forward BFS checks `any_shortcut_found && !any_clean_found`;
    /// the relaxed BFS always returns `false`.
    fn should_prune_all_shortcut(&self, num_qbs: usize) -> bool;

    /// The current shortcut constraints, used for witness refresh and the
    /// `expand_node_children` call.  Returns an empty slice for the relaxed BFS.
    fn shortcut_constraints(&self) -> &[ShortcutConstraints];
}

// ── ForwardBfsCallbacks ───────────────────────────────────────────────────────

pub struct ForwardBfsCallbacks<'a> {
    pub board: &'a QuantumBoard,
    pub paths: &'a mut ShortcutPaths,
    pub shortcut_constraints: &'a mut Vec<ShortcutConstraints>,
    pub shortcut_qstate_set:  &'a mut FxHashSet<QState>,
    pub any_shortcut_found:   &'a mut bool,
    pub any_clean_found:      &'a mut bool,
    pub slot_has_clean:       &'a mut Vec<bool>,
    pub publishable: &'a mut bool,
    pub exit_start_slots: u64,
    pub concrete_exit_covered: bool,
}

impl<'a> BfsCallbacks for ForwardBfsCallbacks<'a> {
    fn on_shortcut_path(&mut self, path: Vec<Dir>) { self.paths.remember(path); }
    fn on_shortcut_found(&mut self, qstate: &QState, slots: &[StartSlot], verbose: u8) {
        *self.any_shortcut_found = true;
        if verbose > 0 {
            println!("Found shortcut at step with qstate {:?}", qstate.cur);
        }
        add_shortcut_qstate(
            self.shortcut_constraints,
            self.shortcut_qstate_set,
            qstate.clone(),
            slots,
        );
        if verbose > 0 {
            println!("Shortcut:");
            for c in self.shortcut_constraints.iter() {
                println!("{:b}", c.invalid_slots_mask);
                for &(a, b) in c.collision_pairs.iter() {
                    println!("  Collision between slot {} and slot {}", a, b);
                }
            }
        }
    }

    fn on_clean_exit(
        &mut self,
        node: &BfsNode,
        pool: &[Assignment],
        slots: &[StartSlot],
        num_qbs: usize,
        shortcut_constraints: &[ShortcutConstraints],
    ) {
        *self.any_clean_found = true;
        if !*self.publishable && !self.concrete_exit_covered {
            let mut uncovered = node.qstate.clone();
            let mut forbidden = self.exit_start_slots;
            while forbidden != 0 {
                let i = forbidden.trailing_zeros() as usize;
                forbidden &= forbidden - 1;
                uncovered.invalidate(i);
            }
            let witness = find_uncovered_witness(
                &uncovered, slots, num_qbs, shortcut_constraints,
            );
            if let Some(assignment) = &witness {
                crate::search_records::capture_witness(self.board, slots, assignment);
            }
            *self.publishable = witness.is_some();
        }
        if let Some(idx) = node.witness_idx {
            for &slot_i in pool[idx].iter() {
                self.slot_has_clean[slot_i as usize] = true;
            }
        }
        for i in 0..slots.len() {
            if self.slot_has_clean[i] {
                continue;
            }
            if !node.qstate.is_valid(i) {
                continue;
            }
            if slot_has_uncovered_assignment(i, &node.qstate, slots, num_qbs, shortcut_constraints) {
                self.slot_has_clean[i] = true;
            }
        }
    }

    fn should_prune_all_shortcut(&self, num_qbs: usize) -> bool {
        num_qbs == 0 && *self.any_shortcut_found && !*self.any_clean_found
    }

    fn shortcut_constraints(&self) -> &[ShortcutConstraints] {
        self.shortcut_constraints
    }
}

// ── forward_bfs ───────────────────────────────────────────────────────────────

/// A state budget may delay a proof, but must never discard a backward branch.
/// Shortcuts from the limited attempt remain valid on this identical board.
pub fn forward_bfs_complete(
    board: &QuantumBoard,
    exit: FlatIdx,
    target_steps: u32,
    path: &[Dir],
    verbose: u8,
    initial_state_limit: Option<u32>,
) -> BfsResult {
    crate::qb_split::solve(board, exit, target_steps, path, verbose, initial_state_limit)
}

pub fn forward_bfs(
    board: &QuantumBoard,
    exit: FlatIdx,
    target_steps: u32,
    path: &[Dir],
    initial_shortcuts: &[Shortcut],
    quick_check_only: bool,
    verbose: u8,
    state_limit: Option<u32>,
) -> BfsResult {
    forward_bfs_impl(board, exit, target_steps, path, initial_shortcuts, quick_check_only,
        verbose, state_limit, None, None, false)
}

pub(crate) fn forward_bfs_until(
    board: &QuantumBoard, exit: FlatIdx, target_steps: u32, path: &[Dir],
    shortcuts: &[Shortcut], verbose: u8, state_limit: Option<u32>,
    deadline: Option<std::time::Instant>,
    paths: Option<&mut ShortcutPaths>,
) -> BfsResult {
    forward_bfs_impl(board, exit, target_steps, path, shortcuts, false,
        verbose, state_limit, paths, deadline, true)
}

/// Revalidate inherited paths in this board's slot space, and retain a bounded
/// selection of newly discovered paths for subsequent backward children.
pub fn forward_bfs_reusing(
    board: &QuantumBoard,
    exit: FlatIdx,
    target_steps: u32,
    path: &[Dir],
    paths: &mut ShortcutPaths,
    verbose: u8,
    state_limit: Option<u32>,
) -> BfsResult {
    let result = forward_bfs_impl(board, exit, target_steps, path, &[], false,
        verbose, state_limit, Some(paths), None, false);
    match result {
        BfsResult::Indeterminate { shortcuts, .. } => {
            forward_bfs_impl(board, exit, target_steps, path, &shortcuts, false,
                verbose, None, Some(paths), None, false)
        }
        complete => complete,
    }
}

fn forward_bfs_impl(
    board: &QuantumBoard,
    exit: FlatIdx,
    target_steps: u32,
    path: &[Dir],
    initial_shortcuts: &[Shortcut],
    quick_check_only: bool,
    verbose: u8,
    state_limit: Option<u32>,
    reuse: Option<&mut ShortcutPaths>,
    deadline: Option<std::time::Instant>,
    adaptive_attempt: bool,
) -> BfsResult {
    let track_paths = reuse.is_some();
    let mut unused_paths = ShortcutPaths::default();
    let paths = reuse.unwrap_or(&mut unused_paths);
    let inherited_paths = std::mem::take(paths);
    if verbose > 0 {
        println!("Starting forward BFS with target_steps={}, quick_check_only={}, state_limit={:?}",
            target_steps, quick_check_only, state_limit);
        println!("Initial board:");
        print!("{}", crate::io::format_board_header(board, exit, "Initial Board"));
        print!("{}", crate::io::format_board(board, exit));
    }

    //if player is already at the exit, we have a shortcut if target_steps > 0, else a clean solution.
    let player = board.find_player();
    if player == exit {
        return if target_steps > 0 {
            BfsResult::AllShortcut
        } else {
            BfsResult::SomeClean {
                ruled_out: vec![],
                shortcuts: vec![],
                publishable: true,
            }
        };
    }

    // ── Build start table ─────────────────────────────────────────────────
    let mut slots: Vec<StartSlot> = Vec::new();
    let mut qb_id_remap: Vec<Option<usize>> = vec![None; board.quantum_blocks.len()];
    let mut num_qbs = 0usize;

    for (orig_id, qb) in board.quantum_blocks.iter().enumerate() {
        if qb.is_empty() {
            continue;
        }
        let compact_id = num_qbs;
        qb_id_remap[orig_id] = Some(compact_id);
        num_qbs += 1;
        let wall_pos = qb.wall_pos;
        for start in qb.all_starts() {
            slots.push(StartSlot {
                qb_id: compact_id as QBlockId,
                start_pos: start,
                is_wall: wall_pos == Some(start),
                push_mask: qb.push_mask_for_start(start),
            });
        }
    }

    let mut static_walls = 0u128;
    let mut static_boxes = 0u128;
    let mut static_inner_mask = 0u128;
    for i in 0..(board.width * board.height) as usize {
        match board.tiles[i] {
            Tile::Wall => static_walls |= 1u128 << i,
            Tile::Box => static_boxes |= 1u128 << i,
            _ => {}
        }
        if board.in_inner(board.coords(i as u16).0, board.coords(i as u16).1) {
            static_inner_mask |= 1u128 << i;
        }
    }

    let mut qb_masks = vec![0u64; board.quantum_blocks.len()];
    for (i, slot) in slots.iter().enumerate() {
        qb_masks[slot.qb_id as usize] |= 1 << i;
    }

    let orig_ids: Vec<usize> = qb_id_remap
        .iter()
        .enumerate()
        .filter_map(|(orig, &compact)| compact.map(|_| orig))
        .collect();

    debug_assert!(
        slots.len() <= MAX_STARTS,
        "Too many QB start slots ({}); increase MAX_STARTS",
        slots.len()
    );

    let player = board.find_player();
    let initial_box_state = BoxState::default();
    let mut initial_qstate = QState::new(&slots);

    for (i, slot) in slots.iter().enumerate() {
        if effective_tile(slot.start_pos, board, &initial_box_state) == Tile::Box {
            initial_qstate.invalidate(i);
        }
    }

    // ── BFS state ─────────────────────────────────────────────────────────

    // shortcut_constraints: compacted constraint representations for every node
    // that reached exit early. An assignment is "covered" iff it matches at
    // least one constraint set.
    let mut shortcut_constraints: Vec<ShortcutConstraints> = Vec::new();
    let mut shortcut_qstate_set: FxHashSet<QState> = FxHashSet::default();
    for s in initial_shortcuts {
        add_shortcut_qstate(
            &mut shortcut_constraints,
            &mut shortcut_qstate_set,
            QState::from_shortcut(s, &slots),
            &slots,
        );
    }

    // For the result: did we ever find an uncovered assignment at the exit
    // at exactly target_steps? Track per-slot, plus global flags for the
    // zero-QB case where slot_has_clean is empty.
    let mut slot_has_clean: Vec<bool> = vec![false; slots.len()];
    let mut any_clean_found = false;
    let mut publishable = false;
    let mut any_shortcut_found = false;

    // Shared assignment pool used by quick checks and the main search.
    let mut assignment_pool = AssignmentPool::default();

    let root_node = BfsNode {
        player,
        steps: 0,
        qstate: initial_qstate.clone(),
        box_state: initial_box_state.clone(),
        witness_idx: None, // Lazy refresh will find one
        //history: SmallVec::new(),
    };

    for candidate in inherited_paths.paths() {
        replay_shortcut(board, exit, target_steps, candidate, &slots, num_qbs, &root_node,
            &mut shortcut_constraints, &mut shortcut_qstate_set, &mut assignment_pool, paths);
    }

    // ── Quick Shortcut Check ──
    quick_shortcut_check(
        board,
        exit,
        target_steps,
        path,
        &slots,
        num_qbs,
        &root_node,
        &mut shortcut_constraints,
        &mut shortcut_qstate_set,
        &mut assignment_pool,
        paths,
        track_paths,
    );
    if !shortcut_constraints.is_empty() {
        any_shortcut_found = true;
    }

    if quick_check_only {
        if num_qbs == 0 {
            return if any_shortcut_found {
                BfsResult::AllShortcut
            } else {
                BfsResult::Indeterminate {
                    ruled_out: vec![],
                    shortcuts: vec![],
                }
            };
        }

        if !shortcut_constraints.is_empty() {
            for i in 0..slots.len() {
                if !initial_qstate.is_valid(i) {
                    continue;
                }
                if !slot_has_uncovered_assignment(
                    i,
                    &initial_qstate,
                    &slots,
                    num_qbs,
                    &shortcut_constraints,
                ) {
                    initial_qstate.invalidate(i);
                }
            }
            if !has_valid_assignment(&initial_qstate, &slots, num_qbs) {
                return BfsResult::AllShortcut;
            }
        }

        let mut ruled_out: Vec<(usize, FlatIdx)> = Vec::new();
        for (i, slot) in slots.iter().enumerate() {
            if !initial_qstate.is_valid(i) {
                let orig_qb = orig_ids[slot.qb_id as usize];
                ruled_out.push((orig_qb, slot.start_pos));
                if verbose > 0 {
                    let (x, y) = board.coords(slot.start_pos as u16);
                    println!("Ruled out start position: QB {} at ({}, {})", orig_qb, x, y);
                }
            }
        }

        let mut filtered_shortcut_qstates: Vec<QState> = Vec::new();
        for qs in shortcut_qstate_set.iter() {
            let mut qs = qs.clone();
            for &(orig_qb_id, start_pos) in &ruled_out {
                if let Some(slot_idx) = slots
                    .iter()
                    .position(|s| orig_ids[s.qb_id as usize] == orig_qb_id && s.start_pos == start_pos)
                {
                    qs.invalidate(slot_idx);
                }
            }
            if has_valid_assignment(&qs, &slots, num_qbs) {
                filtered_shortcut_qstates.push(qs);
            }
        }

        let final_shortcuts = filtered_shortcut_qstates
            .into_iter()
            .map(|qs| qs.to_shortcut(&slots))
            .filter(|s| !s.entries.is_empty())
            .collect();

        return BfsResult::Indeterminate {
            ruled_out,
            shortcuts: final_shortcuts,
        };
    }

    // ── Early QB Resolution ──
    if num_qbs > 0 && !shortcut_constraints.is_empty() {
        let mut invalidated_any = false;
        for i in 0..slots.len() {
            if !initial_qstate.is_valid(i) {
                continue;
            }
            if !slot_has_uncovered_assignment(
                i,
                &initial_qstate,
                &slots,
                num_qbs,
                &shortcut_constraints,
            ) {
                initial_qstate.invalidate(i);
                invalidated_any = true;
            }
        }
        if invalidated_any && !has_valid_assignment(&initial_qstate, &slots, num_qbs) {
            return BfsResult::AllShortcut;
        }
    }

    let initial_witness_idx = match find_uncovered_witness(&initial_qstate, &slots, num_qbs, &shortcut_constraints) {
        Some(w) => Some(assignment_pool.intern(w)),
        None => return BfsResult::AllShortcut,
    };

    let mut visited = SimpleVisited::new();
    if track_paths && num_qbs > 0 {
        visited.paths = Some(PathTracker::new(state_key(player, &initial_qstate, &root_node.box_state)));
    }

    //let mut relaxed_cache = RelaxedCache::new();
    let mut ctx = SearchContext {
        q_occupancy: 0,
        box_occupancy: 0,
        covered_mask: 0,
        static_walls,
        static_boxes,
        box_origin_groups: BoxState::origin_groups(board),
        static_inner_mask,
        pushable: [0; 4],
        relaxed_cache: RelaxedCache::new(),
        #[cfg(feature = "walking-dominance")]
        walks: Default::default(),
        exit_distances: wall_distances(board, exit),
    };

    let cap = target_steps as usize + 1;
    let mut buckets: Vec<Vec<BfsNode>> = vec![Vec::new(); cap];
    buckets[0].push(BfsNode {
        witness_idx: initial_witness_idx,
        qstate: initial_qstate.clone(),
        box_state: initial_box_state,
        ..root_node
    });

    // ── Main loop ─────────────────────────────────────────────────────────
    //
    // `callbacks` is scoped to this block so its mutable borrows of
    // `shortcut_constraints` and `shortcut_qstate_set` are released before
    // we pass those to `build_indeterminate_from_shortcuts` below.

    let mut coverage_witness = initial_witness_idx;
    let mut checked_shortcuts = shortcut_qstate_set.len();
    let state_limit_hit = {
        let mut callbacks = ForwardBfsCallbacks {
            board,
            paths,
            shortcut_constraints: &mut shortcut_constraints,
            shortcut_qstate_set:  &mut shortcut_qstate_set,
            any_shortcut_found:   &mut any_shortcut_found,
            any_clean_found:      &mut any_clean_found,
            slot_has_clean:       &mut slot_has_clean,
            publishable: &mut publishable,
            exit_start_slots: slots.iter().enumerate().fold(0, |mask, (i, slot)| {
                mask | if slot.start_pos == exit { 1u64 << i } else { 0 }
            }),
            concrete_exit_covered: board.get_flat(exit) == Tile::Box,
        };

        let mut limit_hit = false;
        'search: for step in 0..cap {
            let mut bucket = std::mem::take(&mut buckets[step]);
            while let Some(node) = bucket.pop() {
                let key = node_key(&node);
                if !visited.insert(&key, node.steps) {
                    continue;
                }
                process_bfs_node(
                    node,
                    &key,
                    step as u32,
                    target_steps,
                    exit,
                    board,
                    &slots,
                    num_qbs,
                    &qb_masks,
                    &mut ctx,
                    &mut visited,
                    &mut callbacks,
                    &mut buckets,
                    &mut assignment_pool,
                    true,
                    verbose,
                );
                // A global coverage proof makes every pending state redundant.
                // Amortize checks and retain a witness while it stays uncovered.
                if visited.len() % 256 == 0
                    && callbacks.shortcut_qstate_set.len() != checked_shortcuts
                {
                    checked_shortcuts = callbacks.shortcut_qstate_set.len();
                    coverage_witness = refresh_witness_in_pool(
                        coverage_witness, &mut assignment_pool, &initial_qstate,
                        &slots, num_qbs, callbacks.shortcut_constraints,
                    );
                    if coverage_witness.is_none() {
                        return BfsResult::AllShortcut;
                    }
                }
                if visited.len() % 256 == 0 && deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                    limit_hit = true;
                    break 'search;
                }
            }
            // This depth will never be used again; release its backing allocation.
            drop(bucket);

            if let Some(limit) = state_limit {
                if visited.len() > limit as usize {
                    limit_hit = true;
                    break;
                }
            }
        }
        limit_hit
        // `callbacks` drops here, releasing mutable borrows.
    };

    if state_limit_hit {
        if adaptive_attempt {
            // The adaptive caller never applies partial slot exclusions. Keep
            // proofs only for an identical-board retry; splits replay paths.
            let shortcuts = if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
                Vec::new()
            } else {
                shortcut_qstate_set.iter().map(|qs| qs.to_shortcut(&slots)).collect()
            };
            return BfsResult::Indeterminate { ruled_out: Vec::new(), shortcuts };
        }
        return build_indeterminate_from_shortcuts(
            &initial_qstate,
            &slots,
            num_qbs,
            &shortcut_constraints,
            &shortcut_qstate_set,
            &orig_ids,
        );
    }

    // ── Compute result ────────────────────────────────────────────────────

    // Zero-QB case: there is exactly one assignment (the empty one).
    // It's a shortcut iff any_shortcut_found and not any_clean_found.
    if num_qbs == 0 {
        return if !any_clean_found {
            if any_shortcut_found {
                BfsResult::AllShortcut
            } else {
                BfsResult::Unreachable
            }
        } else {
            BfsResult::SomeClean {
                ruled_out: vec![],
                publishable,
                shortcuts: shortcut_qstate_set
                    .iter()
                    .cloned()
                    .map(|qs| qs.to_shortcut(&slots))
                    .filter(|s| !s.entries.is_empty())
                    .collect(),
            }
        };
    }

    // If no clean exit was ever reached, return Unreachable or AllShortcut.
    if !any_clean_found {
        if shortcut_constraints.is_empty() {
            return BfsResult::Unreachable;
        }
        return BfsResult::AllShortcut;
    }

    // Build ruled_out: slots with no clean assignment.
    let mut ruled_out: Vec<(usize, FlatIdx)> = Vec::new();
    for (i, (&clean, slot)) in slot_has_clean.iter().zip(slots.iter()).enumerate() {
        if !clean {
            if verbose > 0 {
                let (x, y) = board.coords(slot.start_pos as u16);
                println!("Ruled out start position: QB {} at ({}, {})", orig_ids[slot.qb_id as usize], x, y);
            }
            let orig_qb = orig_ids[slot.qb_id as usize];
            ruled_out.push((orig_qb, slot.start_pos));
        }
    }

    if adaptive_attempt {
        return BfsResult::SomeClean { ruled_out, publishable, shortcuts: Vec::new() };
    }

    // Filter shortcut qstates: remove any that become invalid after applying ruled_out.
    // This ensures that simple constraints (ruled_out) take precedence over complex ones (shortcuts).
    let mut filtered_shortcut_qstates: Vec<QState> = Vec::new();
    for qs in shortcut_qstate_set.iter() {
        let mut qs = qs.clone();
        // Apply ruled_out to this specific shortcut QState
        for &(orig_qb_id, start_pos) in &ruled_out {
            // Find the slot index corresponding to this (orig_qb_id, start_pos)
            // This is a bit inefficient, but the number of ruled_out entries should be small.
            if let Some(slot_idx) = slots
                .iter()
                .position(|s| orig_ids[s.qb_id as usize] == orig_qb_id && s.start_pos == start_pos)
            {
                qs.invalidate(slot_idx);
            }
        }
        // If, after applying ruled_out, this QState still has valid assignments, keep it.
        if has_valid_assignment(&qs, &slots, num_qbs) {
            filtered_shortcut_qstates.push(qs);
        }
    }

    // Convert filtered QStates to stable Shortcuts and remove any that become empty.
    let final_shortcuts = filtered_shortcut_qstates
        .into_iter()
        .map(|qs| qs.to_shortcut(&slots))
        .filter(|s| !s.entries.is_empty()) // Filter out truly empty shortcuts
        .collect();

    BfsResult::SomeClean {
        ruled_out,
        shortcuts: final_shortcuts,
        publishable,
    }
}

fn build_indeterminate_from_shortcuts(
    initial_qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcut_constraints: &[ShortcutConstraints],
    shortcut_qstate_set: &FxHashSet<QState>,
    orig_ids: &[usize],
) -> BfsResult {
    if num_qbs == 0 {
        return BfsResult::Indeterminate {
            ruled_out: Vec::new(),
            shortcuts: Vec::new(),
        };
    }

    let mut qstate = initial_qstate.clone();
    for i in 0..slots.len() {
        if !qstate.is_valid(i) {
            continue;
        }
        if !slot_has_uncovered_assignment(i, &qstate, slots, num_qbs, shortcut_constraints) {
            qstate.invalidate(i);
        }
    }

    let mut ruled_out: Vec<(usize, FlatIdx)> = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        if !qstate.is_valid(i) {
            let orig_qb = orig_ids[slot.qb_id as usize];
            ruled_out.push((orig_qb, slot.start_pos));
        }
    }

    let mut filtered_shortcut_qstates: Vec<QState> = Vec::new();
    for qs in shortcut_qstate_set.iter() {
        let mut qs = qs.clone();
        for &(orig_qb_id, start_pos) in &ruled_out {
            if let Some(slot_idx) = slots
                .iter()
                .position(|s| orig_ids[s.qb_id as usize] == orig_qb_id && s.start_pos == start_pos)
            {
                qs.invalidate(slot_idx);
            }
        }
        if has_valid_assignment(&qs, slots, num_qbs) {
            filtered_shortcut_qstates.push(qs);
        }
    }

    let final_shortcuts = filtered_shortcut_qstates
        .into_iter()
        .map(|qs| qs.to_shortcut(slots))
        .filter(|s| !s.entries.is_empty())
        .collect();

    BfsResult::Indeterminate {
        ruled_out,
        shortcuts: final_shortcuts,
    }
}

#[inline(never)]
pub fn process_bfs_node<C: BfsCallbacks>(
    mut node: BfsNode,
    key: &StateKey,
    step: u32,
    target_steps: u32,
    exit: FlatIdx,
    board: &QuantumBoard,
    slots: &[StartSlot],
    num_qbs: usize,
    qb_masks: &[u64],
    ctx: &mut SearchContext,
    visited: &mut dyn VisitedTracker,
    callbacks: &mut C,
    buckets: &mut [Vec<BfsNode>],
    pool: &mut AssignmentPool,
    allow_relaxed: bool,
    verbose: u8,
) {
    let distance = ctx.exit_distances[node.player as usize];
    if distance == u8::MAX || distance as u32 > target_steps - node.steps {
        return;
    }
    #[cfg(feature = "walking-dominance")]
    if allow_relaxed && node.player != exit && ctx.walks.dominates(&node) { return; }
    if verbose > 0 && step < 10 && allow_relaxed{
    //if step < 2 {
        println!("Processing node at step {} with player at ({}, {}), qstate {:?}, box_state {:?}, key {:?}, qstate len {}",
            node.steps, board.coords(node.player).0, board.coords(node.player).1, node.qstate.cur, node.box_state.moved, key, node.qstate.len);
    }

    

    // ── Imputation Pruning ────────────────────────────────────────
    let imputation_verbose = verbose;
    if num_qbs > 0 && if allow_relaxed && cfg!(feature = "indexed-dominance") {
        visited.dominated(&node)
    } else {
        try_prune_by_imputation(&node, visited, board, slots, imputation_verbose)
    } {
        return;
    }

    // ── Pre-compute Occupancy and Coverage ────────────────────────
    ctx.q_occupancy = 0;
    for &p in node.qstate.cur[..node.qstate.len].iter() {
        if p != INVALID_SLOT && p != FREE_SLOT {
            ctx.q_occupancy |= 1u128 << p;
        }
    }

    ctx.box_occupancy = ctx.static_boxes;
    for &(orig, _) in node.box_state.moved.iter() {
        ctx.box_occupancy &= !(1u128 << orig);
    }
    for &(_, dest) in node.box_state.moved.iter() {
        if dest != FREE_SLOT as FlatIdx {
            ctx.box_occupancy |= 1u128 << dest;
        }
    }
    

    // Compute pushable: for each direction, a bitmask of tiles that can be pushed in that direction.
    ctx.pushable = [0; 4];
    let mut remaining = ctx.box_occupancy;
    while remaining != 0 {
        let cur = remaining.trailing_zeros() as FlatIdx;
        remaining &= remaining - 1;
        let orig = node.box_state.moved.iter()
            .find(|&&(_, dest)| dest == cur).map_or(cur, |&(orig, _)| orig);
        let mask = board.get_box_push_mask(orig);
        for dir in Dir::all() {
            if mask & dir.mask() != 0 { ctx.pushable[dir.idx()] |= 1u128 << cur; }
        }
    }
    let mut valid = node.qstate.valid_mask();
    while valid != 0 {
        let i = valid.trailing_zeros() as usize;
        valid &= valid - 1;
        let pos = node.qstate.get(i);
        if pos == FREE_SLOT as FlatIdx { continue; }
        for dir in Dir::all() {
            if slots[i].allows(dir) { ctx.pushable[dir.idx()] |= 1u128 << pos; }
        }
    }

    

    // Calculate bitmask of tiles that MUST be covered (walls, static boxes, moved boxes,
    // and QBs with only one valid current position).
    ctx.covered_mask = ctx.static_walls | ctx.box_occupancy | !ctx.static_inner_mask;
    let valid_mask = node.qstate.valid_mask();
    for &mask in qb_masks {
        let qb_valid = mask & valid_mask;
        if qb_valid == 0 {
            continue;
        }

        let first_idx = qb_valid.trailing_zeros() as usize;
        let pos = node.qstate.get(first_idx);

        let mut all_at_pos = true;
        let mut temp = qb_valid & !(1 << first_idx);
        while temp != 0 {
            let idx = temp.trailing_zeros() as usize;
            if node.qstate.get(idx) != pos {
                all_at_pos = false;
                break;
            }
            temp &= !(1 << idx);
        }
        if all_at_pos && pos != FREE_SLOT as FlatIdx {
            ctx.covered_mask |= 1u128 << pos;
        }
    }

    if !allow_relaxed && node.player != exit {
        let remaining = target_steps - node.steps;
        match crate::bfsrelaxed::deletion_flood(ctx, node.player, exit, board.width, remaining, false).0 {
            RelaxedResult::Deadlock => return,
            RelaxedResult::AtLeast(d) | RelaxedResult::Exact(d) if d > remaining => return,
            _ => {}
        }
    }

    if visited.len() % 1000000 == 0 {
        println!("Visited {} nodes at step {}/{}", visited.len(), step, target_steps);
    }

    // ── Lazy witness refresh ──────────────────────────────────────
    if callbacks.should_prune_all_shortcut(num_qbs) {
        return;
    }
    if let Some(idx) = refresh_witness_in_pool(
        node.witness_idx,
        pool,
        &node.qstate,
        slots,
        num_qbs,
        callbacks.shortcut_constraints(),
    ) {
        node.witness_idx = Some(idx);
    } else {
        return;
    }

    // ── At exit? ──────────────────────────────────────────────────
    if node.player == exit {
        if node.steps < target_steps {
            if let Some(path) = visited.solution_path(key) {
                callbacks.on_shortcut_path(path);
            }
            if num_qbs > 0 {
                callbacks.on_shortcut_found(&node.qstate, slots, verbose);
            } else {
                // zero-QB shortcut: still need to set any_shortcut_found
                callbacks.on_shortcut_found(&node.qstate, slots, verbose);
            }
            return;
        }
        if node.steps == target_steps {
            // Snapshot the constraints slice before the mutable borrow in on_clean_exit.
            // We copy into a local Vec so the borrow checker is happy.
            let constraints_snapshot: Vec<ShortcutConstraints> =
                callbacks.shortcut_constraints().to_vec();
            callbacks.on_clean_exit(&node, pool, slots, num_qbs, &constraints_snapshot);
            return;
        }
    }

    if node.steps >= target_steps {
        return;
    }

    let saved_q_occupancy   = ctx.q_occupancy;
    let saved_box_occupancy = ctx.box_occupancy;
    let saved_covered_mask  = ctx.covered_mask;
    let saved_static_boxes  = ctx.static_boxes;
    let saved_pushable      = ctx.pushable;

    let steps_remaining = target_steps - node.steps;
    //println!("Running relaxed solve with node.steps = {}", node.steps);
    if allow_relaxed{
        let relaxed_result = relaxed_solve(
            board,
            slots,
            node.player,
            exit,
            steps_remaining,
            verbose,
            Some(100_000),
            &node,
            ctx,
        );
        
        // Restore fields mutated by relaxed_solve before any early return.
        ctx.q_occupancy   = saved_q_occupancy;
        ctx.box_occupancy = saved_box_occupancy;
        ctx.covered_mask  = saved_covered_mask;
        ctx.static_boxes  = saved_static_boxes;
        ctx.pushable      = saved_pushable;

        match relaxed_result {
            RelaxedResult::Deadlock => {
                //println!("Deadlock!");
                return;
            }
            RelaxedResult::AtLeast(lower_bound) | RelaxedResult::Exact(lower_bound) if lower_bound > steps_remaining => {
                //println!("Too high lower bound!");
                return
            },
            RelaxedResult::AtLeast(_) | RelaxedResult::Exact(_) => {
                //println!("Result was exact or lower bound");
            }
        }
    }

    expand_node_children(
        &node,
        key,
        target_steps,
        exit,
        board,
        slots,
        num_qbs,
        callbacks.shortcut_constraints(),
        buckets,
        pool,
        ctx,
        visited,
        allow_relaxed,
    );
}

#[inline(never)]
pub fn expand_node_children(
    node: &BfsNode,
    parent_key: &StateKey,
    target_steps: u32,
    exit: FlatIdx,
    board: &QuantumBoard,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    buckets: &mut [Vec<BfsNode>],
    pool: &mut AssignmentPool,
    ctx: &mut SearchContext,
    visited: &mut dyn VisitedTracker,
    _record_walks: bool,
) {
    #[cfg(feature = "profile-search")]
    let _timer = crate::search_profile::Timer::start(4);
    let mut dists = [u8::MAX; MAX_TILES];
    let mut queue = [0u16; MAX_TILES];
    let (mut head, mut tail) = (0, 0);

    let solid_mask = ctx.static_walls | ctx.box_occupancy;
    let mut slots_at = [0u64; MAX_TILES];
    let mut valid = node.qstate.valid_mask();
    while valid != 0 {
        let i = valid.trailing_zeros() as usize;
        valid &= valid - 1;
        let pos = node.qstate.cur[i];
        if pos != FREE_SLOT { slots_at[pos as usize] |= 1u64 << i; }
    }

    dists[node.player as usize] = 0;
    queue[tail] = node.player;
    tail += 1;

    while head < tail {
        let curr = queue[head];
        head += 1;
        let d = dists[curr as usize];
        let steps_at_curr = node.steps + d as u32;
        let distance = ctx.exit_distances[curr as usize];
        if distance == u8::MAX || steps_at_curr + distance as u32 > target_steps {
            continue;
        }

        if curr == exit {
            if steps_at_curr <= target_steps {
                let mut exit_node = node.clone();
                exit_node.player = exit;
                exit_node.steps = steps_at_curr;
                if visited.record_parent(&exit_node, parent_key, slots) {
                    visited.record_path(&exit_node, parent_key, curr, None, &dists, board.width);
                    buckets[steps_at_curr as usize].push(exit_node);
                }
            }
            continue;
        }

        let (cx, cy) = board.coords(curr);
        for dir in Dir::all() {
            if let Some((nx, ny)) = dir.apply(cx, cy) {
                let next = board.flat(nx, ny);
                if (1u128 << next) & ctx.static_inner_mask == 0 {
                    continue;
                }

                if dists[next as usize] == u8::MAX {
                    let bit = 1u128 << next;
                    let is_solid = (solid_mask & bit) != 0;
                    let is_quantum = (ctx.q_occupancy & bit) != 0;

                    if !is_solid && !is_quantum {
                        dists[next as usize] = d + 1;
                        queue[tail] = next;
                        tail += 1;
                    } else if steps_at_curr + 1 <= target_steps {
                        let new_steps = steps_at_curr + 1;
                        let dist_to_exit = ctx.exit_distances[next as usize];
                        if dist_to_exit != u8::MAX && dist_to_exit as u32 <= target_steps - new_steps {
                            let old_len = buckets[new_steps as usize].len();
                            try_push(
                                board, slots, node, parent_key, next, dir, new_steps,
                                num_qbs, shortcuts, buckets, pool, ctx, visited,
                                slots_at[next as usize],
                            );
                            if buckets[new_steps as usize].len() > old_len {
                                let child = buckets[new_steps as usize].last().unwrap();
                                visited.record_path(child, parent_key, curr, Some(dir), &dists, board.width);
                            }
                        }
                    }
                }
            }
        }
    }
    #[cfg(feature = "walking-dominance")]
    if _record_walks { ctx.walks.record(node, &dists); }
}

/// An inherited path is only a hint until it reaches the exit in this board.
fn replay_shortcut(
    board: &QuantumBoard,
    exit: FlatIdx,
    target_steps: u32,
    path: &[Dir],
    slots: &[StartSlot],
    num_qbs: usize,
    root: &BfsNode,
    constraints: &mut Vec<ShortcutConstraints>,
    states: &mut FxHashSet<QState>,
    pool: &mut AssignmentPool,
    paths: &mut ShortcutPaths,
) {
    let mut node = root.clone();
    // A longer hint may reach the exit before its end; replay only the useful prefix.
    for (i, &dir) in path.iter().take(target_steps.saturating_sub(1) as usize).enumerate() {
        let mut children = simulate_step(&node, dir, board, slots, num_qbs, exit, false, pool);
        let Some(child) = children.pop() else { return; };
        debug_assert!(children.is_empty(), "QB step simulation is deterministic");
        if child.player == exit {
            if add_shortcut_qstate(constraints, states, child.qstate, slots) {
                paths.remember(path[..=i].to_vec());
            }
            return;
        }
        node = child;
    }
}

/// Quickly checks deviations followed by suffixes of the intended solution.
fn quick_shortcut_check(
    board: &QuantumBoard,
    exit: FlatIdx,
    target_steps: u32,
    intended_path: &[Dir],
    slots: &[StartSlot],
    num_qbs: usize,
    root: &BfsNode,
    shortcut_constraints: &mut Vec<ShortcutConstraints>,
    shortcut_qstate_set: &mut FxHashSet<QState>,
    pool: &mut AssignmentPool,
    paths: &mut ShortcutPaths,
    collect_paths: bool,
) {
    if intended_path.is_empty() {
        return;
    }

    let run_suffix = |start_nodes: SmallVec<[BfsNode; 2]>,
                      prefix: &[Dir],
                      suffix: &[Dir],
                      shortcut_constraints: &mut Vec<ShortcutConstraints>,
                      shortcut_qstate_set: &mut FxHashSet<QState>,
                      verbose: bool,
                      pool: &mut AssignmentPool,
                      paths: &mut ShortcutPaths| {
        let mut current = start_nodes.to_vec();
        for (i, &dir) in suffix.iter().enumerate() {
            let mut next_gen = Vec::new();
            for n in current {
                for next in simulate_step(&n, dir, board, slots, num_qbs, exit, verbose, pool) {
                    if verbose{
                        println!("At step {} with player at {}", next.steps, next.player);
                    }
                    if next.player == exit {
                        if next.steps < target_steps {
                            let novel = add_shortcut_qstate(shortcut_constraints, shortcut_qstate_set, next.qstate, slots);
                            if novel && collect_paths {
                                let mut path = prefix.to_vec();
                                path.extend_from_slice(&suffix[..=i]);
                                paths.remember(path);
                            }
                        }
                    } else {
                        if verbose{
                            println!("Pushing next gen");
                        }
                        next_gen.push(next);
                        if verbose{
                            println!("Length of next gen: {}", next_gen.len());
                        }
                    }
                }
            }
            if verbose{
                println!("Length of next gen before current assignment: {}", next_gen.len());
            }
            current = next_gen;
            if current.is_empty() {
                if verbose{
                    println!("Breaking because empty");
                }
                break;
            }
        }
    };
    //compute player_x and player_y
    let (player_x, player_y) = board.coords(root.player);

    //start with any length 3 path, then take any suffix
    for d1 in Dir::all() {
        let next_pos = d1.apply(player_x, player_y);
        if next_pos.is_none() {
            continue;
        }
        let (step_1_x, step_1_y) = next_pos.unwrap();
        let n1s = simulate_step(root, d1, board, slots, num_qbs, exit, false, pool);
        if n1s.is_empty() {
            continue;
        }
        for d2 in Dir::all() {
            let next_pos = d2.apply(step_1_x, step_1_y);
            if next_pos.is_none() {
                continue;
            }
            let (step_2_x, step_2_y) = next_pos.unwrap();
            let n2s = simulate_step(&n1s[0], d2, board, slots, num_qbs, exit, false, pool);
            if n2s.is_empty() {
                continue;
            }
            for d3 in Dir::all() {
                let next_pos = d3.apply(step_2_x, step_2_y);
                if next_pos.is_none() {
                    continue;
                }
                let (step_3_x, step_3_y) = next_pos.unwrap();
                let n3s = simulate_step(&n2s[0], d3, board, slots, num_qbs, exit, false, pool);
                if n3s.is_empty() {
                    continue;
                }
                let (mut path_x, mut path_y) = board.coords(root.player);
                for start_ind in 0..intended_path.len(){
                    let next_pos = intended_path[start_ind].apply(path_x, path_y);
                    if next_pos.is_none() {
                        panic!("Shouldn't happen");
                    }
                    (path_x, path_y) = next_pos.unwrap();
                    //println!("Running suffix for d123 {:?} {:?} {:?}, start_ind {}", d1, d2, d3, start_ind);
                    let verbose = false;//d1 == d2 && d2 == d3 && d1 == Dir::Down && start_ind == 4;
                    if start_ind > 3 && path_x == step_3_x && path_y == step_3_y{
                        /*if start_ind == 5 {
                            println!("{} {:?} {:?} {:?} {} {} {} {}", start_ind, d1, d2, d3, player_x, player_y, try_match_x, try_match_y);
                            println!("{} {}", board.coords(root.player).0, board.coords(root.player).1);
                            println!("{:?} {:?} {:?} {:?} {:?} {:?}", intended_path[0], intended_path[1], intended_path[2], intended_path[3], intended_path[4], intended_path[5]);
                            panic!("Need to debug");
                        }*/
                        //println!("{}", start_ind);
                        run_suffix(
                            n3s.clone(),
                            &[d1, d2, d3],
                            &intended_path[start_ind..],
                            shortcut_constraints,
                            shortcut_qstate_set,
                            verbose,
                            pool,
                            paths,
                        );
                    }
                }
            }
        }
    }

    
}

fn simulate_step(
    node: &BfsNode,
    dir: Dir,
    board: &QuantumBoard,
    slots: &[StartSlot],
    num_qbs: usize,
    exit: FlatIdx,
    verbose: bool,
    pool: &mut AssignmentPool,
) -> SmallVec<[BfsNode; 2]> {
    let mut results = SmallVec::new();
    let (cx, cy) = board.coords(node.player);

    if verbose {
        println!("Starting step simulation")
    }
    
    // Compute inner_mask locally for this function (called infrequently during quick shortcut check)
    let mut inner_mask = 0u128;
    for y in 1..board.height - 1 {
        for x in 1..board.width - 1 {
            inner_mask |= 1u128 << board.flat(x, y);
        }
    }
    
    let next = match dir.apply(cx, cy) {
        Some((nx, ny)) => {
            let n = board.flat(nx, ny);
            if (1u128 << n) & inner_mask != 0 {
                n
            } else {
                return results;
            }
        }
        None => return results,
    };

    if verbose{
        println!("At this part of step simulation");
    }

    let t = effective_tile(next, board, &node.box_state);
    let occ_exists = any_slots_at(&node.qstate, next);
    let new_steps = node.steps + 1;

    let beyond_opt: Option<FlatIdx> = {
        let (tx, ty) = board.coords(next);
        dir.apply(tx, ty)
            .and_then(|(bx, by)| {
                let b = board.flat(bx, by);
                if (1u128 << b) & inner_mask != 0 {
                    Some(b)
                } else {
                    None
                }
            })
    };

    if verbose{
        println!("This far");
    }

    match t {
        Tile::Wall => {}
        Tile::Floor | Tile::Player | Tile::Quantum(_) => {
                if verbose{
                    println!("In floor, player, quantum section");
                }
                if !occ_exists {
                // This is a player move to an empty tile
                    if let Some(child) = make_child(
                        node.qstate.clone(),
                        node.box_state.clone(),
                        next,
                        new_steps,
                        slots,
                        num_qbs,
                        node.witness_idx,
                        pool,
                        &[],
                    ) {
                    results.push(child);
                }
            } else {
                let mut interact_qs = node.qstate.clone();
                // Player steps into 'next'. Any wall interpretation here is impossible.
                let mask = slot_mask_at(&interact_qs, next);
                for (i, slot) in slots.iter().enumerate() {
                    if (mask >> i) & 1 != 0
                    {
                        if slot.is_wall{
                            interact_qs.invalidate(i);
                        }
                        let box_mask = slot.push_mask;
                        if verbose {
                            println!("Box push mask: {:b}", box_mask);
                        }
                        if (box_mask & dir.mask()) == 0 {
                            interact_qs.invalidate(i);
                        }
                    }
                    //also invalidate if push direction is invalid for this slot
                    
                }
                if let Some(child) = make_child_case4(
                    board,
                    slots,
                    interact_qs,
                    node.box_state.clone(),
                    next,
                    dir,
                    beyond_opt,
                    new_steps,
                    exit,
                    num_qbs,
                    node.witness_idx,
                    pool,
                    &[],
                ) {
                    results.push(child);
                }
            }
        }
        Tile::Box => {
            let mut qstate = node.qstate.clone();
            let mut box_state = node.box_state.clone();
            if let Some(beyond) = beyond_opt {
                let beyond_tile = effective_tile(beyond, board, &box_state);
                match beyond_tile {
                    Tile::Wall | Tile::Box => {}
                    _ => {
                        let orig = box_state.orig_at(next, board).unwrap_or(next);
                        box_state.move_box(orig, beyond);
                        // Move of concrete box crushes any quantum interpretations at 'beyond'.
                        qstate.invalidate_all(beyond);
                        // Check if box can be pushed in this direction by checking push_mask
                        let box_dir_mask = board.get_box_push_mask(orig);
                        if (box_dir_mask & dir.mask()) > 0 {
                            if let Some(child) = make_child(
                                qstate,
                                box_state,
                                next,
                                new_steps,
                                slots,
                                num_qbs,
                                node.witness_idx,
                                pool,
                                &[],
                            ) {
                                results.push(child);
                            }
                        }
                    }
                }
            }
        }
    }
    results
}

/// Attempt to find if a "more general" version of the current node has been visited.
/// A more general version is identical but has one or two INVALID slots restored
/// to a valid current position.
fn try_prune_by_imputation(
    node: &BfsNode,
    visited: &dyn VisitedTracker,
    _board: &QuantumBoard,
    slots: &[StartSlot],
    verbose: u8,
) -> bool {
    #[cfg(feature = "profile-search")]
    let _timer = crate::search_profile::Timer::start(0);
    // Identify invalid slots and their possible "valid" positions according to the board.
    if verbose > 1 {
        println!(
            "Trying imputation pruning for node with qstate {:?} and box_state {:?}",
            node.qstate.cur, node.box_state.moved
        );
    }
    let possible = crate::qstate::imputation_candidates(&node.qstate, slots);

    if possible.is_empty() {
        return false;
    }

    // Only the restored slot bytes vary across probes. Reuse the exact key,
    // avoiding a fresh buffer allocation/copy for every candidate superset.
    let mut test_key = node_key(node);

    // 1-slot imputation: Try re-validating one slot.
    for &(i, pos) in &possible {
        test_key.0[3 + i] = pos as u8;
        if visited.contains(&test_key) {
            return true;
        }
        test_key.0[3 + i] = INVALID_SLOT;
    }

    // 2-slot imputation: Try re-validating two slots simultaneously.
    // We bound this to avoid a performance cliff on states with many invalidations.
    if possible.len() >= 2 && possible.len() <= 6 {
        for a in 0..possible.len() {
            let (i, pos_i) = possible[a];
            test_key.0[3 + i] = pos_i as u8;
            for b in a + 1..possible.len() {
                let (j, pos_j) = possible[b];
                if i == j {
                    continue;
                } // Must be different slots.
                if verbose > 1 {
                    println!(
                        "Trying 2-slot imputation for slots {} and {}, positions {} and {}",
                        i, j, pos_i, pos_j
                    );
                }
                test_key.0[3 + j] = pos_j as u8;
                if visited.contains(&test_key) {
                    return true;
                }
                test_key.0[3 + j] = INVALID_SLOT;
            }
            test_key.0[3 + i] = INVALID_SLOT;
        }
    }

    false
}

// ── try_push ─────────────────────────────────────────────────────────────────

#[inline(never)]
fn try_push(
    board: &QuantumBoard,
    slots: &[StartSlot],
    node: &BfsNode,
    parent_key: &StateKey,
    target: FlatIdx,
    dir: Dir,
    new_steps: u32,
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    buckets: &mut [Vec<BfsNode>],
    pool: &mut AssignmentPool,
    ctx: &mut SearchContext,
    visited: &mut dyn VisitedTracker,
    target_mask: u64,
) {
    let target_bit = 1u128 << target;
    if ctx.static_walls & target_bit != 0 { return; }
    let (x, y) = board.coords(target);
    let beyond = dir.apply(x, y).and_then(|(x, y)| {
        let pos = board.flat(x, y);
        (ctx.static_inner_mask & (1u128 << pos) != 0
            && (ctx.static_walls | ctx.box_occupancy) & (1u128 << pos) == 0)
            .then_some(pos)
    });

    let mut qstate = node.qstate.clone();
    let box_state;
    if ctx.box_occupancy & target_bit != 0 {
        let Some(beyond) = beyond else { return; };
        let orig = node.box_state.orig_at(target, board).unwrap_or(target);
        let mask = board.get_box_push_mask(orig);
        if mask & dir.mask() == 0 { return; }
        let mut moved = node.box_state.clone();
        moved.move_box(orig, beyond);
        moved.canonicalize_group(ctx.box_origin_groups[mask as usize]);
        qstate.invalidate_all(beyond);
        box_state = std::borrow::Cow::Owned(moved);
    } else {
        if target_mask == 0 { return; }
        let mut affected = target_mask;
        while affected != 0 {
            let i = affected.trailing_zeros() as usize;
            affected &= affected - 1;
            if !slots[i].is_wall && slots[i].allows(dir) && beyond.is_some() {
                qstate.set(i, beyond.unwrap());
            } else {
                qstate.invalidate(i);
            }
        }
        box_state = std::borrow::Cow::Borrowed(&node.box_state);
    }
    let key = state_key(target, &qstate, &box_state);
    if visited.redundant(&key, new_steps) { return; }
    let Some(witness_idx) = refresh_witness_in_pool(
        node.witness_idx, pool, &qstate, slots, num_qbs, shortcuts,
    ) else { return; };
    let child = BfsNode {
        player: target, steps: new_steps, qstate,
        box_state: box_state.into_owned(), witness_idx: Some(witness_idx),
    };
    if visited.record_parent_key(&child, key, parent_key) {
        buckets[new_steps as usize].push(child);
    }
}

#[inline(never)]
fn make_child_case4(
    board: &QuantumBoard,
    slots: &[StartSlot],
    mut qstate: QState,
    box_state: BoxState,
    target: FlatIdx,
    dir: Dir,
    beyond_opt: Option<FlatIdx>,
    new_steps: u32,
    exit: FlatIdx,
    num_qbs: usize,
    witness_idx: Option<usize>,
    pool: &mut AssignmentPool,
    shortcuts: &[ShortcutConstraints],
) -> Option<BfsNode> {
    let beyond = match beyond_opt {
        Some(b) => b,
        None => {
            // Cannot push because there is no 'beyond' (edge of inner board).
            // Any interpretation where 'target' was a box is now invalid.
            let mask = slot_mask_at(&qstate, target);
            for (i, slot) in slots.iter().enumerate() {
                if (mask >> i) & 1 != 0 && !slot.is_wall {
                    qstate.invalidate(i);
                }
            }
            return make_child(
                qstate, box_state, target, new_steps, slots, num_qbs, witness_idx, pool, shortcuts,
            );
        }
    };

    let beyond_tile = effective_tile(beyond, board, &box_state);

    match beyond_tile {
        Tile::Wall | Tile::Box => {
            // Cannot push. Any interpretation where 'target' was a box is invalid.
            let mask = slot_mask_at(&qstate, target);
            for (i, slot) in slots.iter().enumerate() {
                if (mask >> i) & 1 != 0 && !slot.is_wall {
                    qstate.invalidate(i);
                }
            }
        }
        _ => {
            // Operation 1: Batch move box interpretations to 'beyond'.
            let mask = slot_mask_at(&qstate, target);
            for (i, slot) in slots.iter().enumerate() {
                if (mask >> i) & 1 != 0 && !slot.is_wall {
                    if slot.allows(dir) {
                        qstate.set(i, beyond); // Individual set for non-wall parts
                    } else {
                        qstate.invalidate(i);
                    }
                }
            }
        }
    }

    make_child(
        qstate, box_state, target, new_steps, slots, num_qbs, witness_idx, pool, shortcuts,
    )
}

/// Attempt to construct a child node. Refreshes the witness against the new
/// qstate and current shortcuts; returns None if no uncovered assignment exists.
#[inline(never)]
fn make_child(
    qstate: QState,
    box_state: BoxState,
    player: FlatIdx,
    steps: u32,
    slots: &[StartSlot],
    num_qbs: usize,
    witness_idx: Option<usize>,
    pool: &mut AssignmentPool,
    shortcuts: &[ShortcutConstraints],
) -> Option<BfsNode> {
    let idx = refresh_witness_in_pool(witness_idx, pool, &qstate, slots, num_qbs, shortcuts)?;
    Some(BfsNode {
        player,
        steps,
        qstate,
        box_state,
        witness_idx: Some(idx),
        //history: SmallVec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};

    #[test]
    fn early_duplicate_probes_do_not_reserve_or_block_cheaper_arrivals() {
        let trackers: Vec<Box<dyn VisitedTracker>> = vec![
            Box::new(SimpleVisited::new()),
            Box::new(crate::bfsrelaxed::VisitedWithParents::new()),
        ];
        for mut visited in trackers {
            let mut node = BfsNode {
                player: 7, steps: 9, qstate: QState::new(&[]),
                box_state: BoxState::default(), witness_idx: None,
            };
            let key = node_key(&node);
            assert!(!visited.redundant(&key, 1));
            // A failed witness check must leave no reservation behind.
            assert!(!visited.redundant(&key, 9));
            assert!(visited.record_parent_key(&node, key.clone(), &key));
            assert!(visited.redundant(&key, 9));
            assert!(visited.redundant(&key, 10));
            assert!(!visited.redundant(&key, 8));
            node.steps = 8;
            assert!(visited.record_parent_key(&node, key.clone(), &key));
            assert!(!visited.insert(&key, 9));
            assert!(visited.insert(&key, 8));
            assert!(visited.redundant(&key, 8));
            assert_eq!(visited.len(), 1);
        }
    }

    #[test]
    fn colliding_legacy_fingerprints_remain_distinct_states() {
        let slots: Vec<_> = [9, 10, 11, 15, 22, 29, 36, 46, 53, 38, 39]
            .into_iter().map(|start_pos| StartSlot {
                qb_id: 0, start_pos, is_wall: false, push_mask: 15,
            }).collect();
        let mut first = BfsNode {
            player: 32, steps: 6, qstate: QState::new(&slots),
            box_state: BoxState::default(), witness_idx: None,
        };
        first.qstate.set(10, 38);
        first.box_state.move_box(32, 39);
        let mut second = first.clone();
        second.steps = 12;
        second.qstate.set(7, 54);
        second.qstate.set(8, 54);

        // These are actual arrivals from the 5x7 failure, before/after moving C.
        let legacy_hash = |node: &BfsNode| {
            let mut h = rustc_hash::FxHasher::default();
            node.player.hash(&mut h);
            node.qstate.hash(&mut h);
            node.box_state.hash(&mut h);
            h.finish()
        };
        assert_eq!(legacy_hash(&first), legacy_hash(&second));
        let a = node_key(&first);
        let b = node_key(&second);
        assert_ne!(a, b);

        let mut visited = SimpleVisited::new();
        assert!(visited.record_parent(&first, &a, &slots));
        assert!(visited.insert(&a, first.steps));
        assert!(visited.record_parent(&second, &a, &slots));
        assert!(!visited.contains(&b));
        assert!(visited.insert(&b, second.steps));
        assert_eq!(visited.len(), 2);

        let mut cache = RelaxedCache::new();
        cache.insert(&a, RelaxedResult::Deadlock);
        assert_eq!(cache.get(&b), None);
        cache.insert(&b, RelaxedResult::Exact(20));
        assert_eq!(cache.get(&a), Some(RelaxedResult::Deadlock));
        assert_eq!(cache.get(&b), Some(RelaxedResult::Exact(20)));
    }

    #[test]
    fn reported_solution_pushes_c_right_and_reaches_exit_in_32_steps() {
        let parsed = crate::io::parse_input(include_str!("../tests/fixtures/5x7-unreachable32.txt")).unwrap();
        let board = &parsed.board;
        let slots: Vec<_> = board.quantum_blocks.iter().enumerate().flat_map(|(id, qb)|
            qb.all_starts().into_iter().map(move |start| StartSlot {
                qb_id: id as QBlockId, start_pos: start,
                is_wall: qb.wall_pos == Some(start), push_mask: qb.push_mask_for_start(start),
            })).collect();
        let mut node = BfsNode { player: board.find_player(), steps: 0,
            qstate: QState::new(&slots), box_state: BoxState::default(), witness_idx: None };
        let mut pool = AssignmentPool::default();
        // The route via (1,5) also supports B's lowest starting position.
        for c in "LDLDRUURUULDDDLLLUUURDRRURUULLDD".chars() {
            let dir = match c { 'U' => Dir::Up, 'D' => Dir::Down, 'L' => Dir::Left, _ => Dir::Right };
            let next = simulate_step(&node, dir, board, &slots, board.quantum_blocks.len(),
                parsed.exit, false, &mut pool);
            assert_eq!(next.len(), 1, "failed at {} {c}", node.steps + 1);
            node = next[0].clone();
            if node.steps == 5 {
                assert_eq!(dir, Dir::Right);
                for (i, slot) in slots.iter().enumerate().filter(|(_, s)| s.qb_id == 2) {
                    assert!(slot.allows(Dir::Right));
                    assert_eq!(node.qstate.get(i), board.flat(5, 7));
                }
            }
        }
        assert_eq!(node.steps, 32);
        assert_eq!(node.player, parsed.exit);
        assert_eq!(node.qstate.valid_mask(), (1u64 << slots.len()) - 1);
    }
}
