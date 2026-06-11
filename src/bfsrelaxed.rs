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

    pub fn insert(&mut self, fingerprint: u64, result: RelaxedResult) {
        self.results.insert(fingerprint, result);
    }

    pub fn len(&self) -> usize {
        self.results.len()
    }
}

// ── relaxed_solve ───────────────────────────────────────────────────────────────

pub fn relaxed_solve(
    board: &QuantumBoard,
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

    return RelaxedResult::Exact(steps);

}
