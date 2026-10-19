//! Walking from an expanded state can dominate another arrival at the same
//! box/slot configuration. Costs, rather than just connected regions, matter.
use crate::{
    bfs::{state_key, BfsNode, StateKey},
    board::MAX_TILES,
};
use rustc_hash::FxHashMap;

const MAX_LAYOUTS: usize = 4096;

#[derive(Default)]
pub struct WalkingDominance {
    costs: FxHashMap<StateKey, Box<[u32; MAX_TILES]>>,
}

impl WalkingDominance {
    pub fn dominates(&self, node: &BfsNode) -> bool {
        let key = state_key(0, &node.qstate, &node.box_state);
        self.costs
            .get(&key)
            .is_some_and(|costs| costs[node.player as usize] <= node.steps)
    }

    /// Only record after expansion: pending nodes cannot dominate each other
    /// through paths that have not actually generated their continuations.
    pub fn record(&mut self, node: &BfsNode, distances: &[u8; MAX_TILES]) {
        let key = state_key(0, &node.qstate, &node.box_state);
        if self.costs.len() >= MAX_LAYOUTS && !self.costs.contains_key(&key) {
            return;
        }
        let costs = self
            .costs
            .entry(key)
            .or_insert_with(|| Box::new([u32::MAX; MAX_TILES]));
        for (cost, &distance) in costs.iter_mut().zip(distances) {
            if distance != u8::MAX {
                *cost = (*cost).min(node.steps.saturating_add(u32::from(distance)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        deadlock::BoxState,
        qstate::{QState, StartSlot},
    };

    #[test]
    fn walking_to_exit_still_reports_the_terminal_arrival() {
        let parsed = crate::io::parse_input("exit 3,1\nsteps 2\n400\n").unwrap();
        assert!(matches!(
            crate::bfs::forward_bfs(&parsed.board, parsed.exit, 2, &[], &[], false, 0, None),
            crate::bfs::BfsResult::SomeClean {
                publishable: true,
                ..
            }
        ));
        assert!(matches!(
            crate::bfs::forward_bfs(&parsed.board, parsed.exit, 3, &[], &[], false, 0, None),
            crate::bfs::BfsResult::AllShortcut
        ));
    }

    #[test]
    fn walking_requires_sufficient_cost_and_identical_slot_mapping() {
        let slots = [StartSlot {
            qb_id: 0,
            start_pos: 10,
            is_wall: false,
            push_mask: 15,
        }];
        let mut a = BfsNode {
            player: 7,
            steps: 4,
            qstate: QState::new(&slots),
            box_state: BoxState::default(),
            witness_idx: None,
        };
        let mut distances = [u8::MAX; MAX_TILES];
        distances[7] = 0;
        distances[8] = 1;
        distances[9] = 2;
        let mut index = WalkingDominance::default();
        assert!(!index.dominates(&a));
        index.record(&a, &distances);
        a.player = 9;
        a.steps = 5;
        assert!(!index.dominates(&a));
        a.steps = 6;
        assert!(index.dominates(&a));
        a.player = 10;
        assert!(
            !index.dominates(&a),
            "walking may not cross a QB or other obstacle"
        );
        a.player = 9;
        a.qstate.set(0, 11);
        assert!(!index.dominates(&a));
        a.qstate.set(0, 10);
        a.box_state.move_box(12, 13);
        assert!(!index.dominates(&a));
    }

    #[test]
    fn later_arrival_can_improve_a_different_player_position() {
        let mut node = BfsNode {
            player: 7,
            steps: 4,
            qstate: QState::new(&[]),
            box_state: BoxState::default(),
            witness_idx: None,
        };
        let mut distances = [u8::MAX; MAX_TILES];
        distances[7] = 0;
        distances[9] = 5;
        let mut index = WalkingDominance::default();
        index.record(&node, &distances);
        node.player = 9;
        node.steps = 6;
        assert!(!index.dominates(&node));
        distances[7] = 5;
        distances[9] = 0;
        index.record(&node, &distances);
        assert!(index.dominates(&node));
        node.player = 7;
        node.steps = 4;
        assert!(
            index.dominates(&node),
            "later expansion must preserve earlier cheap arrivals"
        );
    }
}
