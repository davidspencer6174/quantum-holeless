//! Optional, bounded structural dominance within one fixed forward-solve context.
use rustc_hash::FxHashMap;

use crate::bfs::BfsNode;
use crate::deadlock::BoxState;
use crate::qstate::QState;
use crate::types::FlatIdx;

const MAX_REPRESENTATIVES: usize = 16_384;
const MAX_BUCKET: usize = 64;

#[derive(Default)]
pub(crate) struct DominanceIndex {
    buckets: FxHashMap<(FlatIdx, BoxState), Vec<Representative>>,
    len: usize,
}

struct Representative {
    qstate: QState,
    steps: u32,
}

fn dominates(a: &QState, a_steps: u32, b: &QState, b_steps: u32) -> bool {
    if a_steps > b_steps || a.len != b.len || b.valid_mask() & !a.valid_mask() != 0 {
        return false;
    }
    let mut valid = b.valid_mask();
    while valid != 0 {
        let i = valid.trailing_zeros() as usize;
        valid &= valid - 1;
        if a.get(i) != b.get(i) {
            return false;
        }
    }
    true
}

impl DominanceIndex {
    /// Called only for settled nodes, in nondecreasing cost order. Discarding
    /// representatives at either cap loses an optimization, never search states.
    pub(crate) fn check_and_insert(&mut self, node: &BfsNode) -> bool {
        let key = (node.player, node.box_state.clone());
        if let Some(bucket) = self.buckets.get_mut(&key) {
            if bucket
                .iter()
                .any(|r| dominates(&r.qstate, r.steps, &node.qstate, node.steps))
            {
                return true;
            }
            let old_len = bucket.len();
            bucket.retain(|r| !dominates(&node.qstate, node.steps, &r.qstate, r.steps));
            self.len -= old_len - bucket.len();
            if bucket.len() < MAX_BUCKET && self.len < MAX_REPRESENTATIVES {
                bucket.push(Representative {
                    qstate: node.qstate.clone(),
                    steps: node.steps,
                });
                self.len += 1;
            }
        } else if self.len < MAX_REPRESENTATIVES {
            self.buckets.insert(
                key,
                vec![Representative {
                    qstate: node.qstate.clone(),
                    steps: node.steps,
                }],
            );
            self.len += 1;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qstate::StartSlot;

    fn node() -> BfsNode {
        let slots: Vec<_> = (0..6)
            .map(|i| StartSlot {
                qb_id: 0,
                start_pos: i + 10,
                is_wall: false,
                push_mask: 15,
            })
            .collect();
        BfsNode {
            player: 7,
            steps: 4,
            qstate: QState::new(&slots),
            box_state: BoxState::default(),
            witness_idx: None,
        }
    }

    #[test]
    fn arbitrary_superset_positions_and_three_restored_slots() {
        let mut index = DominanceIndex::default();
        let mut a = node();
        for i in 0..3 {
            a.qstate.set(i, 30 + i as u16);
        }
        assert!(!index.check_and_insert(&a));
        let mut b = a.clone();
        for i in 0..3 {
            b.qstate.invalidate(i);
        }
        b.steps += 1;
        assert!(index.check_and_insert(&b));
        b.qstate.set(3, 42);
        assert!(!index.check_and_insert(&b));
    }

    #[test]
    fn dominance_respects_cost_player_boxes_and_slot_identity() {
        let a = node();
        let mut b = a.clone();
        b.qstate.invalidate(0);
        assert!(!dominates(&a.qstate, 5, &b.qstate, 4));
        assert!(dominates(&a.qstate, 4, &b.qstate, 4));
        let mut index = DominanceIndex::default();
        index.check_and_insert(&a);
        b.player += 1;
        assert!(!index.check_and_insert(&b));
        b.player = a.player;
        b.box_state.move_box(50, 51);
        assert!(!index.check_and_insert(&b));
        b.box_state = a.box_state.clone();
        b.qstate.set(1, a.qstate.get(2));
        b.qstate.set(2, a.qstate.get(1));
        assert!(!index.check_and_insert(&b));
    }

    #[test]
    fn equal_cost_superset_replaces_but_later_superset_keeps_earlier_subset() {
        let a = node();
        let mut b = a.clone();
        b.qstate.invalidate(0);
        let mut index = DominanceIndex::default();
        index.check_and_insert(&b);
        index.check_and_insert(&a);
        assert_eq!(index.len, 1);
        let mut index = DominanceIndex::default();
        index.check_and_insert(&b);
        let mut later = a;
        later.steps += 1;
        index.check_and_insert(&later);
        assert_eq!(index.len, 2);
    }

    #[test]
    fn a_full_bucket_only_loses_optional_representatives() {
        let mut index = DominanceIndex::default();
        for pos in 1..=MAX_BUCKET + 1 {
            let mut a = node();
            a.qstate.set(0, pos as FlatIdx);
            assert!(!index.check_and_insert(&a));
        }
        assert_eq!(index.len, MAX_BUCKET);
        let mut dropped = node();
        dropped.qstate.set(0, (MAX_BUCKET + 1) as FlatIdx);
        assert!(!index.check_and_insert(&dropped));
        let mut retained = node();
        retained.qstate.set(0, 1);
        retained.qstate.invalidate(5);
        assert!(index.check_and_insert(&retained));
    }
}
