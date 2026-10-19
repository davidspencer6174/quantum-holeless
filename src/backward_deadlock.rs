//! A necessary condition for ever publishing a backward descendant.
use crate::{
    board::QuantumBoard,
    types::{Dir, FlatIdx, Tile},
};

/// Remove all walls and QBs, allow the player to stand anywhere, and delete
/// every concrete box with space for a pull. A surviving exit box is frozen
/// even in this relaxation. Backward moves cannot remove it without a pull.
/// Push masks are ignored because backward pulls may add new directions.
pub(crate) fn exit_is_pull_frozen(board: &QuantumBoard, exit: FlatIdx) -> bool {
    if board.get_flat(exit) != Tile::Box {
        return false;
    }
    let mut boxes = 0u128;
    for (pos, tile) in board.tiles.iter().enumerate() {
        if *tile == Tile::Box {
            boxes |= 1u128 << pos;
        }
    }
    loop {
        let before = boxes;
        let mut remaining = boxes;
        while remaining != 0 {
            let pos = remaining.trailing_zeros() as FlatIdx;
            remaining &= remaining - 1;
            let (x, y) = board.coords(pos);
            for dir in Dir::all() {
                let Some((ax, ay)) = dir.apply(x, y) else {
                    continue;
                };
                let Some((bx, by)) = dir.apply(ax, ay) else {
                    continue;
                };
                if !board.in_inner(ax, ay) || !board.in_inner(bx, by) {
                    continue;
                }
                let support = (1u128 << board.flat(ax, ay)) | (1u128 << board.flat(bx, by));
                if boxes & support == 0 {
                    if pos == exit {
                        return false;
                    }
                    boxes &= !(1u128 << pos);
                    break;
                }
            }
        }
        if before == boxes {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};

    #[test]
    fn odd_lattice_is_frozen_but_removing_one_block_can_unlock_it() {
        let mut board = QuantumBoard::new(5, 5);
        for y in (1..=5).step_by(2) {
            for x in (1..=5).step_by(2) {
                board.set(x, y, Tile::Box);
            }
        }
        let exit = board.flat(3, 3);
        assert!(exit_is_pull_frozen(&board, exit));
        board.set(3, 1, Tile::Floor);
        assert!(!exit_is_pull_frozen(&board, exit));
    }

    #[test]
    fn relaxation_ignores_walls_qbs_masks_and_allows_cascading_deletions() {
        let mut board = QuantumBoard::new(4, 1);
        board.set(1, 1, Tile::Box);
        board.set(2, 1, Tile::Box);
        let exit = board.flat(1, 1);
        // The box at 2 must be deleted before the exit box can be deleted.
        assert!(!exit_is_pull_frozen(&board, exit));
        board.add_quantum_block([board.flat(3, 1), board.flat(4, 1)], None);
        assert!(!exit_is_pull_frozen(&board, exit));
        let mut narrow = QuantumBoard::new(3, 1);
        narrow.set(2, 1, Tile::Box);
        assert!(exit_is_pull_frozen(&narrow, narrow.flat(2, 1)));
        assert!(!exit_is_pull_frozen(&narrow, narrow.flat(1, 1)));
    }

    #[test]
    fn frozen_exits_remain_covered_under_all_small_board_pull_sequences() {
        // Independent exact pull search, with teleporting player and no walls.
        // Every 3x3 box layout and every initially occupied exit is checked.
        let mut board = QuantumBoard::new(3, 3);
        let cells: Vec<_> = (1..=3).flat_map(|y| (1..=3).map(move |x| (x, y))).collect();
        for layout in 1u16..512 {
            for (i, &(x, y)) in cells.iter().enumerate() {
                board.set(
                    x,
                    y,
                    if layout & (1 << i) != 0 {
                        Tile::Box
                    } else {
                        Tile::Floor
                    },
                );
            }
            let frozen: Vec<_> = cells
                .iter()
                .enumerate()
                .filter_map(|(i, &(x, y))| {
                    exit_is_pull_frozen(&board, board.flat(x, y)).then_some(i)
                })
                .collect();
            if frozen.is_empty() {
                continue;
            }
            let mut seen = HashSet::from([layout]);
            let mut queue = VecDeque::from([layout]);
            while let Some(state) = queue.pop_front() {
                for &i in &frozen {
                    assert_ne!(state & (1 << i), 0, "layout={layout} exit={i}");
                }
                for i in 0..9 {
                    if state & (1 << i) == 0 {
                        continue;
                    }
                    let x = (i % 3) as i32;
                    let y = (i / 3) as i32;
                    for (dx, dy) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
                        let (ax, ay, bx, by) = (x + dx, y + dy, x + 2 * dx, y + 2 * dy);
                        if !(0..3).contains(&bx) || !(0..3).contains(&by) {
                            continue;
                        }
                        let a = ay * 3 + ax;
                        let b = by * 3 + bx;
                        if state & ((1 << a) | (1 << b)) != 0 {
                            continue;
                        }
                        let next = (state & !(1 << i)) | (1 << a);
                        if seen.insert(next) {
                            queue.push_back(next);
                        }
                    }
                }
            }
        }
    }
}
