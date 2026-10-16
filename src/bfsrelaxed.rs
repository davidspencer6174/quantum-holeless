// Relaxed reachability oracle for forward BFS pruning.
//
// This solver intentionally answers a different question from the exact
// forward BFS: can an easier version of this state reach the exit? If even the
// easier state cannot reach the exit, the exact state is deadlocked.

use rustc_hash::{FxHashSet, FxHashMap};

use crate::bfs::{BfsCallbacks, BfsNode, SearchContext,
                 VisitedTracker, process_bfs_node};
use crate::board::{QuantumBoard};
use crate::deadlock::BoxState;
use crate::qstate::{
    Assignment, QState,
    ShortcutConstraints, StartSlot, FREE_SLOT, INVALID, INVALID_SLOT,
};
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
                        if new_bound > *existing_bound {
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
    nodes: FxHashMap<u64, VisitRecord>,
    settled_count: usize,
}

struct VisitRecord {
    cost: u32,
    parent: Option<u64>,
    settled: bool,
}

impl VisitedWithParents {
    pub fn new() -> Self {
        Self {
            nodes: FxHashMap::default(),
            settled_count: 0,
        }
    }

    pub fn get_parent(&self, fp: u64) -> Option<u64> {
        self.nodes.get(&fp).and_then(|record| record.parent)
    }
}

impl VisitedTracker for VisitedWithParents {
    fn contains(&self, fp: u64) -> bool {
        self.nodes.get(&fp).map_or(false, |record| record.settled)
    }

    fn insert(&mut self, fp: u64, steps: u32) -> bool {
        let record = self.nodes.entry(fp).or_insert(VisitRecord {
            cost: steps,
            parent: None,
            settled: false,
        });
        if record.settled || steps > record.cost {
            return false;
        }
        debug_assert_eq!(record.cost, steps);
        record.settled = true;
        self.settled_count += 1;
        true
    }

    fn len(&self) -> usize {
        self.settled_count
    }

    fn record_parent(&mut self, child: &BfsNode, parent_fp: u64, slots: &[StartSlot]) -> bool {
        let steps = child.steps;
        let child_fp = fingerprint_relaxed(child.player, &child.qstate, slots, &child.box_state);
        if self.nodes.get(&child_fp).map_or(false, |record| record.settled || record.cost <= steps) {
            return false;
        }
        self.nodes.entry(child_fp)
            .and_modify(|record| {
                if !record.settled && steps < record.cost {
                    record.cost = steps;
                    record.parent = Some(parent_fp);
                }
            })
            .or_insert(VisitRecord {
                cost: steps,
                parent: Some(parent_fp),
                settled: false,
            });
        true
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

pub fn qstate_to_relaxed_version(qstate: &QState, slots: &[StartSlot]) -> Vec<(u8, u8, u8)> {
    let mut id_curpos_to_mask_map = FxHashMap::default();
    for (i, slot) in slots.iter().enumerate() {
        if qstate.get(i) == FREE_SLOT as u16 || qstate.get(i) == INVALID {
            continue;
        }
        let key = (slot.qb_id as u8, qstate.get(i) as u8);

        let current_mask = id_curpos_to_mask_map.get(&key).unwrap_or(&0);
        
        id_curpos_to_mask_map.insert(key, current_mask | slot.push_mask);
    }
    let mut out_vec = Vec::new();
    for (key, mask) in id_curpos_to_mask_map.iter() {
        out_vec.push(((*key).0, (*key).1, *mask));
    }
    //sort out_vec to canonicalize
    out_vec.sort();
    out_vec
}

#[inline(never)] // Mark this explicitely, as it's a known hotspot we want to measure
pub fn fingerprint_relaxed(player: FlatIdx, qstate: &QState, slots: &[StartSlot], box_state: &BoxState) -> u64 {
    // Slots and static geometry are fixed for one forward solve. Preserve slot
    // identity: OR-ing masks at a position loses which moves remain compatible.
    debug_assert_eq!(qstate.len, slots.len());
    crate::bfs::fingerprint_raw(player, qstate, box_state)
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

/// Optimistically delete pushable blocks. Core searches only need the first
/// exit touch; outer searches also need the full closure for selecting a core.
pub(crate) fn deletion_flood(
    ctx: &SearchContext, player: FlatIdx, exit: FlatIdx, width: u16,
    target_steps: u32, complete_closure: bool,
) -> (RelaxedResult, u128) {
    let mut visited = 1u128 << player;
    if player == exit { return (RelaxedResult::Exact(0), visited); }
    let mut covered = ctx.covered_mask;
    let mut steps = 0;
    let mut exit_distance = None;
    loop {
        if steps > target_steps {
            return (exit_distance.map_or(RelaxedResult::AtLeast(steps), RelaxedResult::Exact), visited);
        }
        let floor = !covered;
        let up = (visited >> width) & (floor | (ctx.pushable[Dir::Up.idx()] & !(covered << width)));
        let down = (visited << width) & (floor | (ctx.pushable[Dir::Down.idx()] & !(covered >> width)));
        let left = (visited >> 1) & (floor | (ctx.pushable[Dir::Left.idx()] & !(covered << 1)));
        let right = (visited << 1) & (floor | (ctx.pushable[Dir::Right.idx()] & !(covered >> 1)));
        let added = (up | down | left | right) & !visited;
        steps += 1;
        if added & (1u128 << exit) != 0 {
            exit_distance = Some(steps);
            if !complete_closure { return (RelaxedResult::Exact(steps), visited | added); }
        }
        if added == 0 {
            return (exit_distance.map_or(RelaxedResult::Deadlock, RelaxedResult::Exact), visited);
        }
        covered &= !added;
        visited |= added;
    }
}

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
    #[cfg(feature = "profile-search")]
    let _timer = crate::search_profile::Timer::start(3);

    //if player is already at the exit, return Exact(0)
    if player == exit {
        return RelaxedResult::Exact(0);
    }

    let (flood_result, visited_mask) = deletion_flood(ctx, player, exit, board.width, target_steps, true);
    let exit_touch_steps = match flood_result {
        RelaxedResult::Exact(distance) if distance <= target_steps => distance,
        other => return other,
    };

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
        return RelaxedResult::Exact(exit_touch_steps);
    }

    let mut new_root_node = root_node.clone();
    new_root_node.steps = 0; // reset steps to 0 for the relaxed solve, which measures from this point forward

    // Collect QB IDs that have at least one current position in the non_core_region,
    // and move all such slots (and concrete boxes) to FREE_SLOT.
    let mut deleted_qb_ids: Vec<QBlockId> = Vec::new();
    let mut deleted_block_counts = 0;

    for (i, slot) in slots.iter().enumerate() {
        let pos = new_root_node.qstate.cur[i];
        if pos != INVALID_SLOT && pos != FREE_SLOT && non_core_region & (1u128 << pos) != 0{
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
            // Remap to FREE_SLOT in box_state.moved. If the box was never
            // moved, we need to insert a new entry to record the displacement.
            deleted_block_counts += 1;
            if let Some(entry) = new_root_node.box_state.moved.iter_mut().find(|&&mut (o, _)| o == orig) {
                entry.1 = FREE_SLOT as u16;
            } else {
                new_root_node.box_state.moved.push((orig, FREE_SLOT as u16));
            }
        }
    }
    if deleted_block_counts + deleted_qb_ids.len() <= 3{
        return RelaxedResult::Exact(exit_touch_steps);
    }
    
    // Keep the original concrete-box table fixed. Deleted boxes are represented
    // by FREE_SLOT in BoxState, just like boxes moved to another physical cell.
    new_root_node.box_state.moved.sort_unstable_by_key(|&(orig, _)| orig);

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
                        if node.steps + bound > target_steps {
                            continue;
                        }
                    }
                    RelaxedResult::Exact(dist) => {
                        let total_cost = node.steps + dist;
                        if total_cost > target_steps {
                            continue;
                        }
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
        drop(bucket);

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
        // Exhausting the inclusive budget proves a strictly larger distance.
        for (&fp, record) in &visited.nodes {
            if record.settled {
                ctx.relaxed_cache.insert(fp, RelaxedResult::AtLeast(target_steps - record.cost + 1));
            }
        }
        //println!("Returning AtLeast target_steps");
        return RelaxedResult::AtLeast(target_steps.saturating_add(1));
    } else{
        for (&fp, record) in &visited.nodes {
            if record.settled {
                ctx.relaxed_cache.insert(fp, RelaxedResult::AtLeast(best_steps.unwrap_or(0).saturating_sub(record.cost)));
            }
        }
        for fp in best_steps_leaf_set.iter() {
            // Store the VALUE, not the reference. (Assuming *fp can be copied/cloned)
            let mut cur = *fp; 
            
            loop {
                let cost = visited.nodes[&cur].cost;
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
    _board: &QuantumBoard,
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
    let possible = crate::qstate::imputation_candidates(&node.qstate, slots);

    if possible.is_empty() {
        return None;
    }

    let mut test_qs = node.qstate.clone();

    // 1-slot imputation: Try re-validating one slot.
    for &(i, pos) in &possible {
        test_qs.set(i, pos); // Still need set() here for specific imputation
        let result = cache.get(fingerprint_relaxed(node.player, &test_qs, slots, &node.box_state));
        if result.is_some(){
            return result.map(imputed_bound);
        }
        test_qs.invalidate(i);
    }

    // 2-slot imputation: Try re-validating two slots simultaneously.
    // We bound this to avoid a performance cliff on states with many invalidations.
    if possible.len() >= 2 && possible.len() <= 6 {
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
                    return result.map(imputed_bound);
                }
                test_qs.invalidate(j);
            }
            test_qs.invalidate(i);
        }
    }

    None
}

fn imputed_bound(result: RelaxedResult) -> RelaxedResult {
    match result {
        // Restoring choices makes a state easier: its solution is only a lower
        // bound for the restricted state, not necessarily a feasible suffix.
        RelaxedResult::Exact(distance) => RelaxedResult::AtLeast(distance),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PUSH_LEFT, PUSH_RIGHT};

    #[test]
    fn cache_keeps_strongest_proven_bound() {
        let mut cache = RelaxedCache::new();
        cache.insert(1, RelaxedResult::AtLeast(5));
        cache.insert(1, RelaxedResult::AtLeast(9));
        cache.insert(1, RelaxedResult::AtLeast(3));
        assert_eq!(cache.get(1), Some(RelaxedResult::AtLeast(9)));
        cache.insert(1, RelaxedResult::Exact(12));
        cache.insert(1, RelaxedResult::AtLeast(10));
        assert_eq!(cache.get(1), Some(RelaxedResult::Exact(12)));
    }

    #[test]
    fn restored_choices_only_prove_a_lower_bound() {
        assert_eq!(imputed_bound(RelaxedResult::Exact(7)), RelaxedResult::AtLeast(7));
        assert_eq!(imputed_bound(RelaxedResult::Deadlock), RelaxedResult::Deadlock);
    }

    #[test]
    fn cache_preserves_slot_masks_and_deleted_qbs() {
        let slots: Vec<_> = [PUSH_LEFT, PUSH_RIGHT, PUSH_LEFT | PUSH_RIGHT].iter()
            .enumerate().map(|(i, &push_mask)| StartSlot {
                qb_id: 0, start_pos: i as FlatIdx + 10, is_wall: false, push_mask,
            }).collect();
        let mut split = QState::new(&slots);
        split.set(0, 12);
        split.set(1, 12);
        split.invalidate(2);
        let mut combined = QState::new(&slots);
        combined.invalidate(0);
        combined.invalidate(1);
        combined.set(2, 12);
        // Same OR-ed masks, but only the combined interpretation can push both ways.
        assert_eq!(qstate_to_relaxed_version(&split, &slots), qstate_to_relaxed_version(&combined, &slots));
        let boxes = BoxState::default();
        assert_ne!(fingerprint_relaxed(9, &split, &slots, &boxes), fingerprint_relaxed(9, &combined, &slots, &boxes));
        let mut absent = combined.clone();
        let mut impossible = combined.clone();
        for i in 0..slots.len() {
            absent.set(i, FREE_SLOT as FlatIdx);
            impossible.invalidate(i);
        }
        assert_ne!(fingerprint_relaxed(9, &absent, &slots, &boxes), fingerprint_relaxed(9, &impossible, &slots, &boxes));
    }

    #[test]
    fn weighted_parent_edges_follow_the_best_arrival() {
        let mut tracker = VisitedWithParents::new();
        let mut child = BfsNode {
            player: 12, steps: 9, qstate: QState::new(&[]),
            box_state: BoxState::default(), witness_idx: None,
        };
        let fp = fingerprint_relaxed(child.player, &child.qstate, &[], &child.box_state);
        tracker.insert(1, 0);
        tracker.record_parent(&child, 1, &[]);
        tracker.insert(2, 2);
        child.steps = 5;
        tracker.record_parent(&child, 2, &[]);
        assert_eq!(tracker.get_parent(fp), Some(2));
        assert!(!tracker.insert(fp, 9));
        assert!(tracker.insert(fp, 5));
        assert!(!tracker.insert(fp, 5));
        assert_eq!(tracker.len(), 3);
    }

    #[test]
    fn core_search_prunes_a_push_onto_the_exit_and_reuses_its_bound() {
        use crate::types::{PUSH_DOWN, PUSH_UP, Tile};
        let mut board = QuantumBoard::new(5, 5);
        for y in 1..=3 {
            for x in 1..=5 { board.set(x, y, Tile::Floor); }
        }
        board.set(1, 3, Tile::Player);
        for x in 1..=4 {
            board.set(x, 2, Tile::Box);
            board.set_box_push_mask(board.flat(x, 2), PUSH_UP);
        }
        board.set(2, 4, Tile::Box);
        board.set_box_push_mask(board.flat(2, 4), PUSH_DOWN);
        board.set(2, 5, Tile::Floor);
        let exit = board.flat(2, 5);
        let mut ctx = SearchContext {
            q_occupancy: 0, box_occupancy: 0, covered_mask: 0,
            static_walls: 0, static_boxes: 0, static_inner_mask: 0,
            pushable: [0; 4], relaxed_cache: RelaxedCache::new(),
            exit_distances: crate::bfs::wall_distances(&board, exit),
        };
        for y in 1..=5 {
            for x in 1..=5 {
                let pos = board.flat(x, y);
                let bit = 1u128 << pos;
                ctx.static_inner_mask |= bit;
                match board.get_flat(pos) {
                    Tile::Wall => ctx.static_walls |= bit,
                    Tile::Box => {
                        ctx.static_boxes |= bit;
                        for dir in Dir::all() {
                            if board.get_box_push_mask(pos) & dir.mask() != 0 {
                                ctx.pushable[dir.idx()] |= bit;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        ctx.static_walls |= !ctx.static_inner_mask;
        ctx.box_occupancy = ctx.static_boxes;
        ctx.covered_mask = ctx.static_walls | ctx.static_boxes;
        let root = BfsNode { player: board.find_player(), steps: 0,
            qstate: QState::new(&[]), box_state: BoxState::default(), witness_idx: None };
        let original_boxes = ctx.static_boxes;
        let result = relaxed_solve(&board, &[], root.player, exit, 8, 0, None, &root, &mut ctx);
        assert_eq!(result, RelaxedResult::AtLeast(9));
        assert!(ctx.relaxed_cache.len() > 0, "the core search must actually run");
        assert_eq!(ctx.static_boxes, original_boxes);
        // The public caller restores these scratch masks after the nested search.
        ctx.box_occupancy = original_boxes;
        ctx.covered_mask = ctx.static_walls | original_boxes;
        let cached = relaxed_solve(&board, &[], root.player, exit, 8, 0, None, &root, &mut ctx);
        assert_eq!(cached, RelaxedResult::AtLeast(9));
    }
}
