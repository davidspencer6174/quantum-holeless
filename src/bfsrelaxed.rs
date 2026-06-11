// Relaxed reachability oracle for forward BFS pruning.
//
// This solver intentionally answers a different question from the exact
// forward BFS: can an easier version of this state reach the exit? If even the
// easier state cannot reach the exit, the exact state is deadlocked.

use rustc_hash::{FxHashSet, FxHasher, FxHashMap};
use smallvec::{smallvec, SmallVec};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use crate::bfs::{BfsNode, SearchContext, node_fingerprint, fingerprint_raw,
                 refresh_witness_in_pool};
use crate::board::{QuantumBoard, MAX_TILES};
use crate::deadlock::{effective_tile, is_deadlock, BoxState};
use crate::qstate::{
    any_slots_at, find_uncovered_witness, has_valid_assignment,
    slot_has_uncovered_assignment, slot_mask_at, Assignment, QState, Shortcut,
    ShortcutConstraints, StartSlot, INVALID, INVALID_SLOT, MAX_STARTS,
};
use crate::types::{Dir, FlatIdx, QBlockId, Tile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelaxedResult {
    Deadlock,
    LowerBound(u32),
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
                RelaxedResult::LowerBound(existing_bound) => {
                    if let RelaxedResult::LowerBound(new_bound) = result {
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

pub fn basic_flood_fill(
    blocking_mask: u128,
    start_pos: FlatIdx,
) -> u128 {
    let mut visited_mask = 0u128;
    visited_mask |= 1u128 << start_pos; // mark the starting position as visited
    loop {
        let mut new_visited_mask = visited_mask;
        for dir in Dir::all().iter() {
            let delta = match dir {
                Dir::Up => -(MAX_TILES as i128),
                Dir::Down => MAX_TILES as i128,
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

// ── relaxed_solve ───────────────────────────────────────────────────────────────

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
    //println!("Static inner mask: {:b}", ctx.static_inner_mask);
    //println!("Covered mask: {:b}", ctx.covered_mask);
    //println!("Left mask: {:b}", ctx.pushable[2]);
    loop {
        //println!("Relaxed solve step {}, visited_mask {:032x}, covered_mask {:032x}",
        //    steps, visited_mask, cur_covered_mask);
        if steps > target_steps {
            return RelaxedResult::LowerBound(steps);
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
            break;
        }
        if new_visited_mask == 0 {
            return RelaxedResult::Deadlock; // no new tiles visited, so we're stuck
        }
        visited_mask |= new_visited_mask;
    }

    let player_floor_flood = basic_flood_fill(ctx.static_walls | ctx.covered_mask | ctx.q_occupancy, player);
    let exit_floor_flood = basic_flood_fill(player_floor_flood, exit);
    let non_core_region = !(exit_floor_flood | player_floor_flood);
    let mut full_relaxed_solve = false;

    if non_core_region & (ctx.q_occupancy | ctx.box_occupancy) == 0 {
        return RelaxedResult::Exact(steps);
    }
    
    let mut new_root_node = root_node.clone();
    
    // Collect QB IDs that have at least one current position in the non_core_region,
    // and move all such slots (and concrete boxes) to position 0.
    let mut deleted_qb_ids: Vec<QBlockId> = Vec::new();

    for (qb_id, qb) in board.quantum_blocks.iter().enumerate() {
        if qb.is_empty() {
            continue;
        }
        let touches_non_core = qb
            .current_positions()
            .any(|pos| (non_core_region >> pos) & 1 != 0);
        if touches_non_core {
            deleted_qb_ids.push(qb_id as QBlockId);
            // Zero out every slot belonging to this QB.
            for (i, slot) in slots.iter().enumerate() {
                if slot.qb_id == qb_id as QBlockId {
                    new_root_node.qstate.set(i, 0);
                }
            }
        }
    }

    // Handle concrete boxes (Tile::Box). These aren't tracked in QStates so we
    // clear them from the SearchContext occupancy instead. Zero out their bit in
    // ctx.box_occupancy so the player can pass through freely.
    ctx.box_occupancy &= !non_core_region;
    ctx.static_boxes &= !non_core_region; // keep ctx consistent if used later

    for (_, dest) in new_root_node.box_state.moved.iter_mut() {
        let current_pos = *dest; // dest is where the box is now
        if (non_core_region >> current_pos) & 1 != 0 {
            *dest = 0;
            full_relaxed_solve = true; // deleted a box; run the full relaxed solve
        }
    }
    if !full_relaxed_solve {
        return RelaxedResult::Exact(steps);
    }

    let mut assignment_pool: Vec<Assignment> = Vec::new();

    //track visited, parents, and cost from parent
    let mut visited_with_parents: FxHashMap<u64, Option<(u64, u32)>> = FxHashMap::default();
    visited_with_parents.insert(node_fingerprint(&new_root_node), None);

    let cap = target_steps as usize + 1;
    let mut buckets: Vec<Vec<BfsNode>> = vec![Vec::new(); cap];
    buckets[0].push(new_root_node);

    


    RelaxedResult::Exact(steps)
}
