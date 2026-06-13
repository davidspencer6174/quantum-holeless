// Relaxed reachability oracle for forward BFS pruning.
//
// This solver intentionally answers a different question from the exact
// forward BFS: can an easier version of this state reach the exit? If even the
// easier state cannot reach the exit, the exact state is deadlocked.

use rustc_hash::{FxHashSet, FxHashMap, FxHasher};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::bfs::{BfsCallbacks, BfsNode, SearchContext,
                 VisitedTracker, process_bfs_node};
use crate::board::{QuantumBoard};
use crate::deadlock::{effective_tile, BoxState};
use crate::qstate::{
    Assignment, QState,
    ShortcutConstraints, StartSlot, FREE_SLOT, INVALID, INVALID_SLOT,
};
use smallvec::{SmallVec};
use crate::types::{Dir, FlatIdx, QBlockId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelaxedResult {
    Deadlock,
    AtLeast(u32),
    Exact(u32),
}

/// Global cache owned by one `forward_bfs` call.
///
/// The key deliberately omits exit and board dimensions because this cache is
/// scoped to a single forward solve, where those values are invariant.
pub struct RelaxedCache {
    results: FxHashMap<u64, RelaxedResult>,
}

impl RelaxedCache {
    pub fn new() -> Self {
        Self {
            results: FxHashMap::default(),
        }
    }

    pub fn get(&self, fingerprint: u64) -> Option<RelaxedResult> {
        self.results.get(&fingerprint).cloned()
    }

    pub fn insert(&mut self, fingerprint: u64, result: RelaxedResult){
        if let Some(existing) = self.results.get(&fingerprint) {
            //if it's exact or deadlock, can't tighten
            //if it's a lower bound, should take the tighter lower bound
            match existing {
                RelaxedResult::Exact(_) | RelaxedResult::Deadlock => {
                    //assert_eq!(*existing, result, "Inconsistent cache entry for fingerprint {:x}: existing {:?}, new {:?}", fingerprint, existing, result);
                    return;
                },
                RelaxedResult::AtLeast(existing_bound) => {
                    if let RelaxedResult::AtLeast(new_bound) = result {
                        if new_bound < *existing_bound {
                            self.results.insert(fingerprint, result);
                        }
                        return;
                    }
                    if let RelaxedResult::Exact(_) = result {
                        self.results.insert(fingerprint, result);
                        return;
                    }
                }
            }
        }        
        self.results.insert(fingerprint, result);
    }

    pub fn len(&self) -> usize {
        self.results.len()
    }
}

pub struct VisitedWithParents {
    /// Committed nodes — same role as SimpleVisited.
    visited: FxHashSet<u64>,
    /// Costs - number of steps taken so far
    costs: FxHashMap<u64, u32>,
    /// Parent pointers recorded at enqueue time.
    /// Written by record_parent; read during trace-back.
    parents: FxHashMap<u64, u64>,
}

impl VisitedWithParents {
    pub fn new() -> Self {
        Self {
            visited: FxHashSet::default(),
            costs: FxHashMap::default(),
            parents: FxHashMap::default(),
        }
    }

    pub fn get_parent(&self, fp: u64) -> Option<u64> {
        self.parents.get(&fp).copied()
    }
}

impl VisitedTracker for VisitedWithParents {
    fn contains(&self, fp: u64) -> bool {
        self.visited.contains(&fp)
    }

    fn insert(&mut self, fp: u64, steps: u32) -> bool {
        if self.costs.contains_key(&fp) {
            if steps < *self.costs.get(&fp).unwrap() {
                self.costs.insert(fp, steps);
            }
        }
        else{
            self.costs.insert(fp, steps);
        }
        self.visited.insert(fp)
    }

    fn len(&self) -> usize {
        self.visited.len()
    }

    fn record_parent(&mut self, child_fp: u64, parent_fp: u64, steps: u32) {
        //if the parent already exists in fewer steps, don't insert
        if self.costs.contains_key(&parent_fp) {
            if steps >= *self.costs.get(&parent_fp).unwrap() {
                return;
            }
        }
        self.costs.insert(child_fp, steps);
        self.parents.entry(child_fp).or_insert(parent_fp);
    }
}

// ── RelaxedBfsCallbacks ───────────────────────────────────────────────────────
//
// No-op implementation of BfsCallbacks for use inside relaxed_solve.
// The relaxed BFS has no shortcut machinery — it just wants to know whether
// the exit is reachable within target_steps, and needs the parent-pointer
// tree populated via VisitedWithParents.  All shortcut methods are stubs.

pub struct RelaxedBfsCallbacks {
    /// Set to true the first time on_clean_exit is called, so the caller can
    /// distinguish "exit reached at exactly target_steps" from "never reached".
    pub any_clean_found: bool,
}

impl RelaxedBfsCallbacks {
    pub fn new() -> Self {
        Self { any_clean_found: false }
    }
}

impl BfsCallbacks for RelaxedBfsCallbacks {
    fn on_shortcut_found(&mut self, _qstate: &QState, _slots: &[StartSlot], _verbose: u8) {
        // The relaxed BFS doesn't track shortcuts — the whole point is to run
        // without any shortcut constraints so we get the full reachability tree.
    }

    fn on_clean_exit(
        &mut self,
        _node: &BfsNode,
        _pool: &[Assignment],
        _slots: &[StartSlot],
        _num_qbs: usize,
        _shortcut_constraints: &[ShortcutConstraints],
    ) {
        self.any_clean_found = true;
    }

    fn should_prune_all_shortcut(&self, _num_qbs: usize) -> bool {
        // Never prune — the relaxed BFS explores the full space.
        false
    }

    fn shortcut_constraints(&self) -> &[ShortcutConstraints] {
        // No shortcuts: every assignment is a candidate.
        &[]
    }
}

#[inline]
fn hash_triple(a: u8, b: u8, c: u8) -> u64 {
    let mut h = FxHasher::default();
    h.write_u8(a);
    h.write_u8(b);
    h.write_u8(c);
    h.finish()
}

// Different multipliers per "role" in the accumulation
const M: u64 = 0x9e3779b97f4a7c15; // Fibonacci hashing constant

#[inline(never)]
fn fingerprint_relaxed(
    player: FlatIdx,
    qstate: &QState,
    slots: &[StartSlot],
    box_state: &BoxState,
) -> u64 {
    let mut per_entry_mask = [[0u8; 128]; 16]; // [qb_id][pos], stack allocated
    for i in 0..qstate.len {
        let pos = qstate.cur[i];
        if pos == INVALID_SLOT || pos == FREE_SLOT { continue; }
        per_entry_mask[slots[i].qb_id as usize][pos as usize] |= slots[i].push_mask;
    }

    let mut acc = 0u64;
    for qb_id in 0..16usize {
        for pos in 0..qstate.len {
            let mask = per_entry_mask[qb_id][pos];
            if mask != 0 {
                let mut h = FxHasher::default();
                h.write_u8(qb_id as u8);
                h.write_u8(pos as u8);
                h.write_u8(mask);
                acc ^= h.finish().wrapping_mul(M);
            }
        }
    }

    // Mix in player and box_state
    let mut h = FxHasher::default();
    player.hash(&mut h);
    box_state.hash(&mut h);
    acc ^= h.finish();
    acc
}

pub fn basic_flood_fill(
    blocking_mask: u128,
    start_pos: FlatIdx,
    width: u16,
) -> u128 {
    let mut visited_mask = 0u128;
    visited_mask |= 1u128 << start_pos; // mark the starting position as visited
    loop {
        let mut new_visited_mask = visited_mask;
        for dir in Dir::all().iter() {
            let delta = match dir {
                Dir::Up => -(width as i128),
                Dir::Down => width as i128,
                Dir::Left => -1,
                Dir::Right => 1,
            };
            let next = if delta > 0 {
                visited_mask << delta
            } else {
                visited_mask >> (-delta as usize)
            };
            let steppable = !blocking_mask;
            let newly_reachable_mask = next & steppable;
            new_visited_mask |= newly_reachable_mask;
        }
        new_visited_mask &= !visited_mask;
        if new_visited_mask == 0 {
            break; // no new tiles visited, so we're done
        }
        visited_mask |= new_visited_mask;
    }
    visited_mask
}

// ── relaxed_solve ─────────────────────────────────────────────────────────────

pub fn relaxed_solve(
    board: &QuantumBoard,
    slots: &[StartSlot],
    player: FlatIdx,
    exit: FlatIdx,
    target_steps: u32,
    verbose: u8,
    state_limit: Option<u32>,
    root_node: &BfsNode,
    ctx: &mut SearchContext,
) -> RelaxedResult {

    //if player is already at the exit, return Exact(0)
    if player == exit {
        return RelaxedResult::Exact(0);
    }

    //now do an iterative relaxation flood fill:
    //anything not in covered_mask is automatically passable

    let mut visited_mask = 0u128;
    visited_mask |= 1u128 << player; // mark the player's starting position as visited
    let mut cur_covered_mask = ctx.covered_mask; // start with the initial covered mask, which includes static walls and boxes
    let mut steps = 0u32;
    let mut exit_touch_steps = None;
    //println!("Static inner mask: {:b}", ctx.static_inner_mask);
    //println!("Covered mask: {:b}", ctx.covered_mask);
    //println!("Left mask: {:b}", ctx.pushable[2]);
    loop {
        //println!("Relaxed solve step {}, visited_mask {:032x}, covered_mask {:032x}",
        //    steps, visited_mask, cur_covered_mask);
        if steps > target_steps {
            if exit_touch_steps.is_none(){
                return RelaxedResult::AtLeast(steps);
            }
            break;
        }

        let mut new_visited_mask = visited_mask;
        for dir in Dir::all().iter() {
            let dir_idx = dir.idx();
            let delta = match dir {
                Dir::Up => -(board.width as i128),
                Dir::Down => board.width as i128,
                Dir::Left => -1,
                Dir::Right => 1,
            };
            let next = if delta > 0 {
                visited_mask << delta
            } else {
                visited_mask >> (-delta as usize)
            };
            let after_covered = if delta > 0 {
                cur_covered_mask >> delta
            } else {
                cur_covered_mask << (-delta as usize)
            };
            let steppable = (!cur_covered_mask) | (ctx.pushable[dir_idx] & !after_covered);
            let newly_reachable_mask = next & steppable;
            //println!("  dir {:?}: delta {}, p1 {:b}, p2 {:b}, newly_reachable_mask {:b}",
            //    dir, delta, p1, p2, newly_reachable_mask);
            new_visited_mask |= newly_reachable_mask;
        }
        //println!("visited mask: {:b}", visited_mask);
        new_visited_mask &= !visited_mask;
        cur_covered_mask &= !new_visited_mask;
        //println!("  new_visited_mask {:b}", new_visited_mask);

        steps += 1;
        if (new_visited_mask & (1u128 << exit)) != 0 {
            exit_touch_steps = Some(steps);
        }
        if new_visited_mask == 0 && exit_touch_steps.is_none() {
            return RelaxedResult::Deadlock; // no new tiles visited, so we're stuck
        }
        visited_mask |= new_visited_mask;
    }

    let player_floor_flood = basic_flood_fill(ctx.static_walls | ctx.covered_mask | ctx.q_occupancy, player, board.width);
    let exit_floor_flood = basic_flood_fill(player_floor_flood | !ctx.static_inner_mask, exit, board.width);
    //note: & visited_mask because blocks that we never even managed to push in phase 1 shouldn't be considered for removal.
    let non_core_region = !(exit_floor_flood | player_floor_flood) & ctx.static_inner_mask & visited_mask;

    //if target_steps > 36 {
        //   println!("Static walls is {:b}", ctx.static_walls);
        //   println!("Covered mask is {:b}", ctx.covered_mask);
        //   println!("Quantum occupancy is {:b}", ctx.q_occupancy);
        //   println!("Player flood is {:b}", player_floor_flood);
        //   println!("Exit flood is {:b}", exit_floor_flood);
        //   println!("Non core region is {:b}", non_core_region);
    //}

    if non_core_region & (ctx.q_occupancy | ctx.box_occupancy) == 0 {
        return RelaxedResult::Exact(exit_touch_steps.unwrap());
    }

    let mut new_root_node = root_node.clone();
    new_root_node.steps = 0; // reset steps to 0 for the relaxed solve, which measures from this point forward

    // Collect QB IDs that have at least one current position in the non_core_region,
    // and move all such slots (and concrete boxes) to position 0.
    let mut deleted_qb_ids: Vec<QBlockId> = Vec::new();
    let mut deleted_block_counts = 0;

    for (i, slot) in slots.iter().enumerate() {
        if non_core_region & (1 << new_root_node.qstate.cur[i]) != 0{
            if !deleted_qb_ids.contains(&slot.qb_id){
                deleted_qb_ids.push(slot.qb_id);
            }
        }
    }
    for qb_id in deleted_qb_ids.iter() {
        for (i, slot) in slots.iter().enumerate() {
            if slot.qb_id == *qb_id {
                new_root_node.qstate.set(i, FREE_SLOT as u16);
            }
        }
    }
    //println!("New root node qstate: {:?}", new_root_node.qstate);

    // Concrete boxes: iterate static_boxes to find every box's current position,
    // resolving through box_state.moved where applicable.
    let mut static_boxes_remaining = ctx.static_boxes;
    while static_boxes_remaining != 0 {
        let orig = static_boxes_remaining.trailing_zeros() as FlatIdx;
        static_boxes_remaining &= static_boxes_remaining - 1; // clear lowest set bit

        let current = new_root_node.box_state.moved
            .iter()
            .find(|&&(o, _)| o == orig)
            .map_or(orig, |&(_, d)| d);

        if (non_core_region >> current) & 1 != 0 {
            // Remap to position 0 in box_state.moved. If the box was never
            // moved, we need to insert a new entry to record the displacement.
            deleted_block_counts += 1;
            if let Some(entry) = new_root_node.box_state.moved.iter_mut().find(|&&mut (o, _)| o == orig) {
                entry.1 = FREE_SLOT as u16;
            } else {
                new_root_node.box_state.moved.push((orig, FREE_SLOT as u16));
            }
        }
    }
    //if !full_relaxed_solve {
    //    return RelaxedResult::Exact(steps);
    //}
    if deleted_block_counts + deleted_qb_ids.len() <= 3{
        return RelaxedResult::Exact(steps);
    }
    
    ctx.box_occupancy &= !non_core_region;
    ctx.static_boxes &= !non_core_region;

    // ── Derive num_qbs and qb_masks from slots ────────────────────────────
    //
    // These mirror the setup in forward_bfs; we need them to call
    // process_bfs_node correctly.
    let num_qbs = slots
        .iter()
        .map(|s| s.qb_id as usize + 1)
        .max()
        .unwrap_or(0);
    let mut qb_masks = vec![0u64; num_qbs];
    for (i, slot) in slots.iter().enumerate() {
        qb_masks[slot.qb_id as usize] |= 1u64 << i;
    }

    let mut visited = VisitedWithParents::new();
    //println!("Root fingerprint: {:x}", node_fingerprint(&new_root_node));
    let root_fp = fingerprint_relaxed(player, &new_root_node.qstate, slots, &new_root_node.box_state);
    //println!("Size of visited: {}", visited.len());
    let mut cached_result = ctx.relaxed_cache.get(root_fp);
    if cached_result.is_none(){
        cached_result = try_cache_by_imputation(&new_root_node, &mut ctx.relaxed_cache, board, slots, verbose);
        //if cached_result.is_some(){
        //    println!("Imputed cache hit");
        //}
    }
    if let Some(cached_result) = cached_result {
        match cached_result {
            RelaxedResult::Deadlock => {
                return RelaxedResult::Deadlock;
            }
            RelaxedResult::AtLeast(bound) => {
                if bound > target_steps {
                    return RelaxedResult::AtLeast(bound);
                }
            }
            RelaxedResult::Exact(dist) => {
                return RelaxedResult::Exact(dist);
            }
        }
        //println!("Cached");
    } else{
        //println!("Uncached");
    }

    let mut assignment_pool: Vec<Assignment> = Vec::new();
    let cap = target_steps as usize + 1;
    let mut buckets: Vec<Vec<BfsNode>> = vec![Vec::new(); cap];

    //println!("Running relaxed solve on node with qstate cur {:?}, box state {:?}, player position {:?}, target steps {}",
    //new_root_node.qstate.cur, new_root_node.box_state.moved, new_root_node.player, target_steps);
    /*if let Some(cached_result) = cached_result {
        println!("Cached");
    } else {
        println!("Uncached");
    }*/

    buckets[0].push(new_root_node);

    let mut callbacks = RelaxedBfsCallbacks::new();

    

    // ── Main bucketed BFS loop ────────────────────────────────────────────
    //
    // Delegates to process_bfs_node from bfs.rs via the BfsCallbacks trait.
    // RelaxedBfsCallbacks stubs out all shortcut machinery, so no shortcut
    // state needs to be threaded through here.
    // VisitedWithParents.record_parent is called automatically by
    // expand_node_children for every child that is enqueued.

    let mut best_steps = None;
    let mut may_be_possible = false;
    let mut best_steps_leaf_set = FxHashSet::default();

    for step in 0..cap {
        if best_steps.is_some() && best_steps.unwrap() <= step as u32 {
            break;
        }
        let mut bucket = std::mem::take(&mut buckets[step]);
        while let Some(node) = bucket.pop() {
            let fp = fingerprint_relaxed(node.player, &node.qstate, slots, &node.box_state);
            //println!("Processing node with qstate cur {:?}, box state {:?}, player position {:?}, current steps {}, fingerprint {:032x}",
            //node.qstate.cur, node.box_state.moved, node.player, node.steps, fp);
            if !visited.insert(fp, node.steps) {
                continue;
            }
            // Check for exit before expanding.
            if node.player == exit {
                if !visited.contains(fp) {
                    visited.insert(fp, node.steps);
                }
                if best_steps.is_none() || node.steps < best_steps.unwrap() {
                    best_steps = Some(node.steps);
                    best_steps_leaf_set.clear();
                    best_steps_leaf_set.insert(fp);
                } else if node.steps == best_steps.unwrap() {
                    best_steps_leaf_set.insert(fp);
                }
                break;
            }
            let cached_result = ctx.relaxed_cache.get(fp);
            if let Some(cached_result) = cached_result {
                match cached_result {
                    RelaxedResult::Deadlock => {
                        continue;
                    }
                    RelaxedResult::AtLeast(bound) => {
                        may_be_possible = true;
                        if node.steps + bound > target_steps {
                            continue;
                        }
                    }
                    RelaxedResult::Exact(dist) => {
                        may_be_possible = true;
                        let total_cost = node.steps + dist;
                        if best_steps.is_none() || total_cost < best_steps.unwrap() {
                            best_steps = Some(total_cost);
                            best_steps_leaf_set.clear();
                            best_steps_leaf_set.insert(fp);
                        } else if total_cost == best_steps.unwrap() {
                            best_steps_leaf_set.insert(fp);
                        }
                        continue;
                    }
                }
            }


            process_bfs_node(
                node,
                fp,
                step as u32,
                target_steps,
                exit,
                board,
                slots,
                num_qbs,
                &qb_masks,
                ctx,
                &mut visited,
                &mut callbacks,
                &mut buckets,
                &mut assignment_pool,
                false,
                verbose,
            );
        }
        buckets[step] = bucket;

        if let Some(limit) = state_limit {
            if visited.len() > limit as usize {
                // State limit hit; conservatively report AtLeast(0) so the
                // forward BFS doesn't prune on a potentially wrong answer.
                println!("Hit state limit");
                return RelaxedResult::AtLeast(0);
            }
        }
    }

    if best_steps.is_none() {
        //no node was able to lead to a solution in target_steps or shorter.
        //all nodes get an atleast of target_steps - the node's cost
        for fp in visited.visited.iter() {
            let cost_of_node = visited.costs.get(fp).unwrap();
            ctx.relaxed_cache.insert(*fp, RelaxedResult::AtLeast(target_steps - cost_of_node + 1));
        }
        //println!("Returning AtLeast target_steps");
        return RelaxedResult::AtLeast(target_steps);
    } else{
        for fp in visited.visited.iter() {
            //println!("Visited: {:?}, costs: {:?}, parents: {:?}", visited.visited, visited.costs, visited.parents);
            let cost_of_node = visited.costs.get(fp).unwrap();
            ctx.relaxed_cache.insert(*fp, RelaxedResult::AtLeast(best_steps.unwrap_or(0) - cost_of_node));
        }
        for fp in best_steps_leaf_set.iter() {
            // Store the VALUE, not the reference. (Assuming *fp can be copied/cloned)
            let mut cur = *fp; 
            
            loop {
                //println!("Visited: {:?}, costs: {:?}, parents: {:?}", visited.visited, visited.costs, visited.parents);
                let cost = visited.costs.get(&cur).unwrap();
                ctx.relaxed_cache.insert(
                    cur, 
                    RelaxedResult::Exact(best_steps.unwrap_or(0) - cost)
                );

                let parent = visited.get_parent(cur); 
                if parent.is_none() {
                    break;
                }
                let parent_id = parent.unwrap();
                cur = parent_id; 
            }
        }
        //println!("Returning Exact {}", best_steps.unwrap_or(0));
        return RelaxedResult::Exact(best_steps.unwrap_or(0));
    }

    //println!("Deadlock!");

    // Exit was never reached within target_steps.
    //RelaxedResult::Deadlock
}





/// Attempt to find if a "more general" version of the current node has been visited.
/// A more general version is identical but has one or two INVALID slots restored
/// to a valid current position.
fn try_cache_by_imputation(
    node: &BfsNode,
    cache: &mut RelaxedCache,
    board: &QuantumBoard,
    slots: &[StartSlot],
    verbose: u8,
) -> Option<RelaxedResult> {
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
        return None;
    }

    let mut test_qs = node.qstate.clone();

    // 1-slot imputation: Try re-validating one slot.
    for &(i, pos) in &possible {
        test_qs.set(i, pos); // Still need set() here for specific imputation
        let result = cache.get(fingerprint_relaxed(node.player, &test_qs, slots, &node.box_state));
        if result.is_some(){
            return result;
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
                let result = cache.get(fingerprint_relaxed(node.player, &test_qs, slots, &node.box_state));
                if result.is_some(){
                    return result;
                }
                test_qs.set(j, INVALID);
            }
            test_qs.set(i, INVALID);
        }
    }

    None
}