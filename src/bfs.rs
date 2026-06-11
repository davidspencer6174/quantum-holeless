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

use rustc_hash::{FxHashSet, FxHasher};
use smallvec::{smallvec, SmallVec};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use crate::board::{QuantumBoard, MAX_TILES};
use crate::deadlock::{effective_tile, is_deadlock, BoxState};
use crate::qstate::{
    any_slots_at, find_uncovered_witness, has_valid_assignment,
    slot_has_uncovered_assignment, slot_mask_at, Assignment, QState, Shortcut,
    ShortcutConstraints, StartSlot, INVALID, INVALID_SLOT, MAX_STARTS,
};
use crate::bfsrelaxed::{RelaxedCache,RelaxedResult, relaxed_solve};
use crate::types::{Dir, FlatIdx, QBlockId, Tile};

// ── BfsNode ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct BfsNode {
    player: FlatIdx,
    steps: u32,
    pub qstate: QState,
    pub box_state: BoxState,
    /// Index into the shared assignment pool for a valid assignment of `qstate`
    /// not covered by any known shortcut. `None` means not assigned yet; a
    /// lazy refresh will fill it. While the node is in the queue, we guarantee
    /// that this resolves to Some(index) (we prune nodes without a witness).
    witness_idx: Option<usize>,
    //history: SmallVec<[Dir; 16]>,
}

// ── SearchContext ─────────────────────────────────────────────────────────────

pub struct SearchContext {
    pub q_occupancy: u128,
    pub box_occupancy: u128,
    pub covered_mask: u128,
    pub static_walls: u128,
    pub static_boxes: u128,
    pub static_inner_mask: u128,
    /// For each direction, a bitmask of tiles that can be pushed in that direction.
    /// A tile is pushable in a direction if:
    ///   (a) it holds a concrete box whose push_mask includes the direction, OR
    ///   (b) it holds at least one valid QB slot whose push_mask includes the direction.
    pub pushable: [u128; 4],
    pub relaxed_cache: RelaxedCache,
}

// ── Fingerprint ───────────────────────────────────────────────────────────────

pub fn node_fingerprint(node: &BfsNode) -> u64 {
    fingerprint_raw(node.player, &node.qstate, &node.box_state)
}
#[inline(never)] // Mark this explicitely, as it's a known hotspot we want to measure
pub fn fingerprint_raw(player: FlatIdx, qstate: &QState, box_state: &BoxState) -> u64 {
    let mut h = DefaultHasher::new();
    player.hash(&mut h);
    qstate.hash(&mut h);
    box_state.hash(&mut h);
    h.finish()
}
// ── BfsResult ─────────────────────────────────────────────────────────────────

pub enum BfsResult {
    Unreachable,
    AllShortcut,
    SomeClean {
        ruled_out: Vec<(usize, FlatIdx)>,
        shortcuts: Vec<Shortcut>,
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
) {
    // 1. Fast identical-state check via the global hash set
    if shortcut_set.contains(&qstate) {
        return;
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
            return;
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
}

/// Try to refresh a node's witness using a shared pool first, then fall back
/// to a full search. Returns an index into `pool` for a valid, uncovered
/// assignment, or `None` if none exists.
pub fn refresh_witness_in_pool(
    witness_idx: Option<usize>,
    pool: &mut Vec<Assignment>,
    qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
) -> Option<usize> {
    // Fast-path: existing witness index still valid
    if let Some(idx) = witness_idx {
        if idx < pool.len() {
            let a = &pool[idx];
            if crate::qstate::assignment_valid_in(a, qstate) && !crate::qstate::assignment_covered(a, shortcuts) {
                return Some(idx);
            }
        }
    }

    // Check the rest of the pool for a reusable witness.
    for (i, a) in pool.iter().enumerate() {
        if crate::qstate::assignment_valid_in(a, qstate) && !crate::qstate::assignment_covered(a, shortcuts) {
            return Some(i);
        }
    }

    // Fallback: expensive search for a fresh witness.
    if let Some(a) = find_uncovered_witness(qstate, slots, num_qbs, shortcuts, None) {
        pool.push(a);
        if pool.len() % 1000 == 0{
            println!("Witness pool size: {}", pool.len());
        }
        return Some(pool.len() - 1);
    }
    None
}

// ── forward_bfs ───────────────────────────────────────────────────────────────

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
    let mut any_shortcut_found = false;

    // Shared assignment pool used by quick checks and the main search.
    let mut assignment_pool: Vec<Assignment> = Vec::new();

    let root_node = BfsNode {
        player,
        steps: 0,
        qstate: initial_qstate.clone(),
        box_state: initial_box_state.clone(),
        witness_idx: None, // Lazy refresh will find one
        //history: SmallVec::new(),
    };

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
            if !has_valid_assignment(&initial_qstate, &slots, num_qbs, None) {
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
            if has_valid_assignment(&qs, &slots, num_qbs, None) {
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
        if invalidated_any && !has_valid_assignment(&initial_qstate, &slots, num_qbs, None) {
            return BfsResult::AllShortcut;
        }
    }

    let initial_witness_idx = match find_uncovered_witness(&initial_qstate, &slots, num_qbs, &shortcut_constraints, None) {
        Some(w) => { assignment_pool.push(w); Some(assignment_pool.len() - 1) },
        None => return BfsResult::AllShortcut,
    };

    let mut visited: FxHashSet<u64> = FxHashSet::default();

    //let mut relaxed_cache = RelaxedCache::new();
    let mut ctx = SearchContext {
        q_occupancy: 0,
        box_occupancy: 0,
        covered_mask: 0,
        static_walls,
        static_boxes,
        static_inner_mask,
        pushable: [0; 4],
        relaxed_cache: RelaxedCache::new(),
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

    for step in 0..cap {
        let mut bucket = std::mem::take(&mut buckets[step]);
        while let Some(node) = bucket.pop() {
            process_bfs_node(
                node,
                step as u32,
                target_steps,
                exit,
                board,
                &slots,
                num_qbs,
                &qb_masks,
                &mut ctx,
                &mut visited,
                &mut shortcut_constraints,
                &mut shortcut_qstate_set,
                &mut any_shortcut_found,
                &mut any_clean_found,
                &mut slot_has_clean,
                &mut buckets,
                &mut assignment_pool,
                verbose,
            );
        }
        buckets[step] = bucket;

        if let Some(limit) = state_limit {
            if visited.len() > limit as usize {
                return build_indeterminate_from_shortcuts(
                    &initial_qstate,
                    &slots,
                    num_qbs,
                    &shortcut_constraints,
                    &shortcut_qstate_set,
                    &orig_ids,
                );
            }
        }
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
        if has_valid_assignment(&qs, &slots, num_qbs, None) {
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
        if has_valid_assignment(&qs, slots, num_qbs, None) {
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
fn process_bfs_node(
    mut node: BfsNode,
    step: u32,
    target_steps: u32,
    exit: FlatIdx,
    board: &QuantumBoard,
    slots: &[StartSlot],
    num_qbs: usize,
    qb_masks: &[u64],
    ctx: &mut SearchContext,
    visited: &mut FxHashSet<u64>,
    shortcut_constraints: &mut Vec<ShortcutConstraints>,
    shortcut_qstate_set: &mut FxHashSet<QState>,
    any_shortcut_found: &mut bool,
    any_clean_found: &mut bool,
    slot_has_clean: &mut [bool],
    buckets: &mut [Vec<BfsNode>],
    pool: &mut Vec<Assignment>,
    verbose: u8,
) {

    let fp = node_fingerprint(&node);
    if verbose > 0 && step < 5 {
        println!("Processing node at step {} with player at ({}, {}), qstate {:?}, box_state {:?}, fingerprint {:016x}, qstate len {}",
            node.steps, board.coords(node.player).0, board.coords(node.player).1, node.qstate.cur, node.box_state.moved, fp, node.qstate.len);
    }
    

    
    if visited.contains(&fp) {
        //if is_on_target_path {
        //    println!("[TRACE] Target path node pruned by duplicate visited fingerprint!");
        //}
        //println!("Pruned node at step {} with player at ({}, {}), qstate {:?}, box_state {:?} due to visited fingerprint {:016x}",
        //    node.steps, board.coords(node.player).0, board.coords(node.player).1, node.qstate.cur, node.box_state.moved, fp);
        //println!("qstate len is {} with valid_mask {:b}", node.qstate.len, node.qstate.valid_mask());
        return;
    }

    // ── Imputation Pruning ────────────────────────────────────────
    let imputation_verbose = verbose;
    if num_qbs > 0 && try_prune_by_imputation(&node, visited, board, slots, imputation_verbose) {
        //if is_on_target_path {
        //    println!("[TRACE] Target path node pruned by imputation!");
        //}
        return;
    }

    if (verbose > 1) && step < 10 {
        println!("Visiting board at step {} with player at ({}, {}), qstate {:?}, box_state {:?}",
            node.steps, board.coords(node.player).0, board.coords(node.player).1, node.qstate.cur, node.box_state.moved);
        //print!("{}", crate::io::format_board_header(board, exit, &format!("BFS Node (Step {})", node.steps)));
        //print!("{}", crate::io::format_board(board, exit));
    }

    // ── Pre-compute Occupancy and Coverage ────────────────────────
    ctx.q_occupancy = 0;
    for &p in node.qstate.cur[..node.qstate.len].iter() {
        if p != INVALID_SLOT {
            ctx.q_occupancy |= 1u128 << p;
        }
    }

    ctx.box_occupancy = ctx.static_boxes;
    for &(orig, _) in node.box_state.moved.iter() {
        ctx.box_occupancy &= !(1u128 << orig);
    }
    for &(_, dest) in node.box_state.moved.iter() {
        ctx.box_occupancy |= 1u128 << dest;
    }

    // Compute pushable: for each direction, a bitmask of tiles that can be pushed in that direction.
    ctx.pushable = [0; 4];
    for &dir in Dir::all().iter() {
        let dir_idx = dir.idx();
        // Check concrete boxes: iterate through set bits in box_occupancy
        {
            let mut remaining = ctx.box_occupancy;
            while remaining != 0 {
                let cur = remaining.trailing_zeros() as usize as FlatIdx;
                let mut orig = cur;
                //check if any moved box maps to orig and if so, use the original position for push mask checking
                for &(moved_from, moved_to) in node.box_state.moved.iter() {
                    if moved_to == orig {
                        orig = moved_from;
                        break;
                    }
                }
                remaining &= remaining - 1; // Clear the lowest set bit
                let box_mask = board.get_box_push_mask(orig);
                if (box_mask & dir.mask()) > 0 {
                    ctx.pushable[dir_idx] |= 1u128 << cur;
                }
            }
        }
        // Check QB slots
        let valid_mask = node.qstate.valid_mask();
        for (i, slot) in slots.iter().enumerate() {
            if (valid_mask >> i) & 1 != 0 && slot.allows(dir) {
                let pos = node.qstate.get(i);
                ctx.pushable[dir_idx] |= 1u128 << pos;
            }
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
        if all_at_pos {
            ctx.covered_mask |= 1u128 << pos;
        }
    }

    let (px, py) = board.coords(node.player);
    let (ex, ey) = board.coords(exit);

    // Manhattan lower-bound pruning.
    let dist = (px as i32 - ex as i32).unsigned_abs() as u32
        + (py as i32 - ey as i32).unsigned_abs() as u32;
    if dist > target_steps - node.steps {
        return;
    }

    if !visited.insert(fp) {
        return;
    }
    //if verbose > 0 && visited.len() % 100000 == 0 {
    if visited.len() % 1000000 == 0 {
        println!("Visited {} nodes at step {}/{}", visited.len(), step, target_steps);
    }

    // ── Lazy witness refresh ──────────────────────────────────────
    if num_qbs == 0 && *any_shortcut_found && !*any_clean_found {
        return;
    }
    if let Some(idx) = refresh_witness_in_pool(node.witness_idx, pool, &node.qstate, slots, num_qbs, shortcut_constraints) {
        node.witness_idx = Some(idx);
    } else {
        return;
    }

    // ── At exit? ──────────────────────────────────────────────────
    if node.player == exit {
        if node.steps < target_steps {
            *any_shortcut_found = true;
            if num_qbs > 0 {
                if verbose > 0 {
                    println!("Found shortcut at step {} with qstate {:?}",
                        node.steps, node.qstate.cur);
                }
                add_shortcut_qstate(shortcut_constraints, shortcut_qstate_set, node.qstate.clone(), slots);
                if verbose > 0{
                    println!("Shortcut:");
                    for c in shortcut_constraints.iter() {
                        println!("{}", c.invalid_slots_mask);
                        for &(a, b) in c.collision_pairs.iter() {
                            println!("  Collision between slot {} and slot {}", a, b);
                        }
                    }
                }
            }
            return;
        }
        if node.steps == target_steps {
            *any_clean_found = true;
            if let Some(idx) = node.witness_idx {
                for &slot_i in pool[idx].iter() {
                    slot_has_clean[slot_i as usize] = true;
                }
            }
            for i in 0..slots.len() {
                if slot_has_clean[i] {
                    continue;
                }
                if !node.qstate.is_valid(i) {
                    continue;
                }
                if slot_has_uncovered_assignment(i, &node.qstate, slots, num_qbs, shortcut_constraints)
                {
                    slot_has_clean[i] = true;
                }
            }
            return;
        }
    }

    if node.steps >= target_steps {
        return;
    }

    let steps_remaining = target_steps - node.steps;
    match relaxed_solve(
        board,
        slots,
        node.player,
        exit,
        steps_remaining,
        verbose,
        Some(100_000),
        &node,
        ctx,
    ) {
        RelaxedResult::Deadlock => {
            //if node.steps < 15{
            //    println!("DEADLOCK with qstate {:?} and box_state {:?} at step {}, pruning!", node.qstate.cur, node.box_state.moved, node.steps);
            //}
            return;
        }
        RelaxedResult::LowerBound(lower_bound) if lower_bound > steps_remaining => {
            //println!("Lower bound pruning!");
            return
        },
        
        RelaxedResult::LowerBound(_) | RelaxedResult::Exact(_) => {}
    }


    expand_node_children(
        &node,
        target_steps,
        exit,
        board,
        slots,
        num_qbs,
        shortcut_constraints,
        buckets,
        pool,
        ctx,
    );
}

#[inline(never)]
fn expand_node_children(
    node: &BfsNode,
    target_steps: u32,
    exit: FlatIdx,
    board: &QuantumBoard,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    buckets: &mut [Vec<BfsNode>],
    pool: &mut Vec<Assignment>,
    ctx: &mut SearchContext,
) {
    let mut dists = [u8::MAX; MAX_TILES];
    let mut queue = [0u16; MAX_TILES];
    let (mut head, mut tail) = (0, 0);
    let (ex, ey) = board.coords(exit);

    let solid_mask = ctx.static_walls | ctx.box_occupancy;

    dists[node.player as usize] = 0;
    queue[tail] = node.player;
    tail += 1;

    while head < tail {
        let curr = queue[head];
        head += 1;
        let d = dists[curr as usize];
        let steps_at_curr = node.steps + d as u32;

        if curr == exit {
            if steps_at_curr <= target_steps {
                let mut exit_node = node.clone();
                exit_node.player = exit;
                exit_node.steps = steps_at_curr;
                buckets[steps_at_curr as usize].push(exit_node);
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
                        let (nx, ny) = board.coords(next);
                        let dist_to_exit = (nx as i32 - ex as i32).unsigned_abs() as u32
                            + (ny as i32 - ey as i32).unsigned_abs() as u32;
                        if dist_to_exit <= target_steps - new_steps {
                            try_push(
                                board, slots, node, curr, next, dir, new_steps, exit, num_qbs,
                                shortcuts, buckets, pool, target_steps, ctx,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Quickly checks for shortcuts by simulating deviations and suffixes of the intended solution.
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
    pool: &mut Vec<Assignment>,
) {
    if intended_path.is_empty() {
        return;
    }

    let run_suffix = |start_nodes: SmallVec<[BfsNode; 2]>,
                      suffix: &[Dir],
                      shortcut_constraints: &mut Vec<ShortcutConstraints>,
                      shortcut_qstate_set: &mut FxHashSet<QState>,
                      pool: &mut Vec<Assignment>| {
        let mut current = start_nodes.to_vec();
        for &dir in suffix {
            let mut next_gen = Vec::new();
            for n in current {
                for next in simulate_step(&n, dir, board, slots, num_qbs, exit, pool) {
                    if next.player == exit {
                        if next.steps < target_steps {
                            add_shortcut_qstate(shortcut_constraints, shortcut_qstate_set, next.qstate, slots);
                        }
                    } else {
                        next_gen.push(next);
                    }
                }
            }
            current = next_gen;
            if current.is_empty() {
                break;
            }
        }
    };
    //start with any length 3 path, then take any suffix
    for d1 in Dir::all() {
        let n1s = simulate_step(root, d1, board, slots, num_qbs, exit, pool);
        //if n1s.player == exit {
        //    if n1s.steps < target_steps {
        //        add_shortcut_qstate(shortcut_qstates, shortcut_qstate_set, n1s.qstate);
        //    }
        //}
        if n1s.is_empty() {
            continue;
        }
        for d2 in Dir::all() {
            let n2s = simulate_step(&n1s[0], d2, board, slots, num_qbs, exit, pool);
            //if n2s.player == exit {
            //    if n2s.steps < target_steps {
            //        add_shortcut_qstate(shortcut_qstates, shortcut_qstate_set, n2s.qstate);
            //    }
            //}
            if n2s.is_empty() {
                continue;
            }
            for d3 in Dir::all() {
                let n3s = simulate_step(&n2s[0], d3, board, slots, num_qbs, exit, pool);
                //if n3s.player == exit {
                //    if n3s.steps < target_steps {
                //        add_shortcut_qstate(shortcut_qstates, shortcut_qstate_set, n3s.qstate);
                //    }
                //}
                if n3s.is_empty() {
                    continue;
                }
                for start_ind in 4..intended_path.len(){
                    run_suffix(
                        n3s.clone(),
                        &intended_path[start_ind..],
                        shortcut_constraints,
                        shortcut_qstate_set,
                        pool,
                    );
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
    pool: &mut Vec<Assignment>,
) -> SmallVec<[BfsNode; 2]> {
    let mut results = SmallVec::new();
    let (cx, cy) = board.coords(node.player);
    
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

    match t {
        Tile::Wall => {}
        Tile::Floor | Tile::Player | Tile::Quantum(_) => {
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
                        let box_mask = board.get_box_push_mask(next);
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
                        let box_dir_mask = board.get_box_push_mask(next);
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
    visited: &FxHashSet<u64>,
    board: &QuantumBoard,
    slots: &[StartSlot],
    verbose: u8,
) -> bool {
    // Identify invalid slots and their possible "valid" positions according to the board.
    if verbose > 1 {
        println!(
            "Trying imputation pruning for node with qstate {:?} and box_state {:?}",
            node.qstate.cur, node.box_state.moved
        );
    }
    let mut possible = SmallVec::<[(usize, FlatIdx); 16]>::new();
    for i in 0..slots.len() {
        if !node.qstate.is_valid(i) {
            let qb = &board.quantum_blocks[slots[i].qb_id as usize];
            if verbose > 1 {
                println!(
                    "Slot {} (QB {}, start {}) is invalid at position {}, effective tile {:?}",
                    i,
                    slots[i].qb_id,
                    slots[i].start_pos,
                    node.qstate.get(i),
                    effective_tile(slots[i].start_pos, board, &node.box_state)
                );
            }
            //generate list of current positions that the QB can occupy based on which slots of the
            //qstate are from the same QB and valid, and the effective tile at those positions.
            for j in 0..slots.len() {
                if i == j {
                    continue;
                }
                if slots[i].qb_id == slots[j].qb_id && node.qstate.is_valid(j) {
                    let pos_j = node.qstate.get(j);
                    possible.push((i, pos_j));
                    if verbose > 1 {
                        println!("  Slot {} (QB {}, start {}) is valid at position {}, effective tile {:?}",
                            j, slots[j].qb_id, slots[j].start_pos, pos_j,
                            effective_tile(pos_j, board, &node.box_state));
                    }
                }
            }
            //additionally, consider the start slot imputation as a possibility
            possible.push((i, slots[i].start_pos));
            if verbose > 1 {
                println!("  Start slot imputation for slot {} (QB {}, start {}) at position {}, effective tile {:?}",
                    i, slots[i].qb_id, slots[i].start_pos, slots[i].start_pos,
                    effective_tile(slots[i].start_pos, board, &node.box_state));
            }
        }
    }

    if possible.is_empty() {
        return false;
    }

    let mut test_qs = node.qstate.clone();

    // 1-slot imputation: Try re-validating one slot.
    for &(i, pos) in &possible {
        test_qs.set(i, pos); // Still need set() here for specific imputation
        if visited.contains(&fingerprint_raw(node.player, &test_qs, &node.box_state)) {
            return true;
        }
        test_qs.invalidate(i);
    }

    // 2-slot imputation: Try re-validating two slots simultaneously.
    // We bound this to avoid a performance cliff on states with many invalidations.
    if possible.len() >= 2 && possible.len() <= 12 {
        for a in 0..possible.len() {
            let (i, pos_i) = possible[a];
            test_qs.set(i, pos_i);
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
                test_qs.set(j, pos_j);
                if visited.contains(&fingerprint_raw(node.player, &test_qs, &node.box_state)) {
                    //println!("Pruning by imputation: re-validating slots {} and {} leads to a visited state", i, j);
                    return true;
                }
                test_qs.set(j, INVALID);
            }
            test_qs.set(i, INVALID);
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
    from_pos: FlatIdx,
    target: FlatIdx,
    dir: Dir,
    new_steps: u32,
    exit: FlatIdx,
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    buckets: &mut [Vec<BfsNode>],
    pool: &mut Vec<Assignment>,
    target_steps: u32,
    ctx: &mut SearchContext,
) {
    let target_tile = effective_tile(target, board, &node.box_state);

    match target_tile {
        Tile::Wall => return,
        Tile::Floor | Tile::Player | Tile::Quantum(_) => {
            handle_player_move_to_quantum_or_floor(
                board, slots, node, from_pos, target, dir, new_steps, exit, num_qbs, shortcuts,
                buckets, pool, target_steps, ctx,
            );
        }
        Tile::Box => {
            handle_player_move_to_box(
                board, slots, node, from_pos, target, dir, new_steps, exit, num_qbs, shortcuts,
                buckets, pool, target_steps, ctx,
            );
        }
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
    pool: &mut Vec<Assignment>,
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

#[inline(never)]
fn handle_player_move_to_quantum_or_floor(
    board: &QuantumBoard,
    slots: &[StartSlot],
    node: &BfsNode,
    _from_pos: FlatIdx, // Unused in this specific handler
    target: FlatIdx,
    dir: Dir,
    new_steps: u32,
    exit: FlatIdx,
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    buckets: &mut [Vec<BfsNode>],
    pool: &mut Vec<Assignment>,
    target_steps: u32,
    ctx: &mut SearchContext,
) {
    let target_mask = slot_mask_at(&node.qstate, target);
    if target_mask == 0 {
        return;
    } // No quantum blocks at target, so nothing to interact with.

    let mut qstate = node.qstate.clone();
    let box_state = node.box_state.clone(); // Clone here, as it might be modified in make_child_case4

    // Player steps into 'target'. Any wall interpretation of a QB at this tile is now impossible.
    for (i, slot) in slots.iter().enumerate() {
        if (target_mask >> i) & 1 != 0 && slot.is_wall {
            qstate.invalidate(i);
        }
    }

    let beyond_opt: Option<FlatIdx> = {
        let (tx, ty) = board.coords(target);
        dir.apply(tx, ty)
            .and_then(|(bx, by)| {
                let b = board.flat(bx, by);
                if (1u128 << b) & ctx.static_inner_mask != 0 { Some(b) } else { None }
            })
    };

    if let Some(mut child) = make_child_case4(
        board,
        slots,
        qstate,
        box_state,
        target,
        dir,
        beyond_opt,
        new_steps,
        exit,
        num_qbs,
        node.witness_idx,
        pool,
        shortcuts,
    ) {
        // Pre-filter child before pushing to bucket
        if should_push_child_to_bucket(board, &child, target_steps, exit, slots, ctx) {
            //child.history = node.history.clone();
            //child.history.push(dir);
            buckets[new_steps as usize].push(child);
        }
    }
}

#[inline(never)]
fn handle_player_move_to_box(
    board: &QuantumBoard,
    slots: &[StartSlot],
    node: &BfsNode,
    _from_pos: FlatIdx, // Unused in this specific handler
    target: FlatIdx,
    dir: Dir,
    new_steps: u32,
    exit: FlatIdx,
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    buckets: &mut [Vec<BfsNode>],
    pool: &mut Vec<Assignment>,
    target_steps: u32,
    ctx: &mut SearchContext,
) {
    let mut qstate = node.qstate.clone();
    let mut box_state = node.box_state.clone();

    let beyond_opt: Option<FlatIdx> = {
        let (tx, ty) = board.coords(target);
        dir.apply(tx, ty)
            .and_then(|(bx, by)| {
                let b = board.flat(bx, by);
                if (1u128 << b) & ctx.static_inner_mask != 0 { Some(b) } else { None }
            })
    };

    let beyond = match beyond_opt {
        Some(b) => b,
        None => return,
    };
    let beyond_tile = effective_tile(beyond, board, &box_state);

    if beyond_tile.is_solid() {
        return;
    }

    let orig = box_state.orig_at(target, board).unwrap_or(target);
    let box_mask = board.get_box_push_mask(orig);
    if (box_mask & dir.mask()) == 0 {
        return;
    }
    box_state.move_box(orig, beyond);
    // Moving a concrete box into 'beyond' crushes any quantum interpretation there.
    qstate.invalidate_all(beyond);

    if let Some(mut child) = make_child(
        qstate,
        box_state,
        target,
        new_steps,
        slots,
        num_qbs,
        node.witness_idx,
        pool,
        shortcuts,
    ) {
        // Pre-filter child with Manhattan check before pushing to bucket
        if should_push_child_to_bucket(board, &child, target_steps, exit, slots, ctx) {
            //child.history = node.history.clone();
            //child.history.push(dir);
            buckets[new_steps as usize].push(child);
        }
    }
}

/// Pre-filter check: Apply Manhattan distance check to a child node before pushing to bucket.
/// Returns true if the child should be pushed, false if it fails the check.
#[inline(never)]
fn should_push_child_to_bucket(
    board: &QuantumBoard,
    child: &BfsNode,
    target_steps: u32,
    exit: FlatIdx,
    slots: &[StartSlot],
    ctx: &mut SearchContext,
) -> bool {
    let steps_remaining = target_steps.saturating_sub(child.steps);
    //compute Manhattan distance to exit
    let (px, py) = board.coords(child.player);
    let (ex, ey) = board.coords(exit);
    let dist = (px as i32 - ex as i32).unsigned_abs() as u32
        + (py as i32 - ey as i32).unsigned_abs() as u32;
        if dist > steps_remaining {
            return false; // Prune by Manhattan distance
        }
    return true;
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
    pool: &mut Vec<Assignment>,
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



