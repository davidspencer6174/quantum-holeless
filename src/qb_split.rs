//! Adaptive splitting is a search strategy, never a pruning rule. Every fixed
//! choice is solved completely before its result contributes to the union.
use crate::search_log::println;
use crate::{
    bfs::{forward_bfs_until, BfsResult},
    board::QuantumBoard,
    shortcut_paths::ShortcutPaths,
    types::{Dir, FlatIdx, Tile},
};
use std::time::{Duration, Instant};

fn split_candidate(board: &QuantumBoard) -> Option<usize> {
    // Prefer direction-rich alternatives per branch. Long, single-direction
    // QBs were poor split choices in the slow-board corpus despite more slots.
    board
        .quantum_blocks
        .iter()
        .enumerate()
        .filter_map(|(id, qb)| {
            let count = qb.num_positions();
            if count < 2
                || !qb
                    .cells
                    .iter()
                    .all(|(pos, starts)| starts.len() == 1 && starts[0] == *pos)
            {
                return None;
            }
            let extra_directions: u32 = qb
                .current_positions()
                .filter(|&pos| qb.wall_pos != Some(pos))
                .map(|pos| qb.box_push_mask_at(pos).count_ones().saturating_sub(1))
                .sum();
            (extra_directions > 0).then_some((id, extra_directions, count as u32))
        })
        .max_by(|a, b| (a.1 * b.2).cmp(&(b.1 * a.2)).then_with(|| b.0.cmp(&a.0)))
        .map(|(id, _, _)| id)
}

pub(crate) fn solve(
    board: &QuantumBoard,
    exit: FlatIdx,
    target: u32,
    path: &[Dir],
    verbose: u8,
    state_limit: Option<u32>,
) -> BfsResult {
    solve_with_budget(
        board,
        exit,
        target,
        path,
        verbose,
        state_limit,
        Duration::from_millis(500),
    )
}

fn solve_with_budget(
    board: &QuantumBoard,
    exit: FlatIdx,
    target: u32,
    path: &[Dir],
    verbose: u8,
    state_limit: Option<u32>,
    budget: Duration,
) -> BfsResult {
    solve_with_hints(
        board,
        exit,
        target,
        path,
        verbose,
        state_limit,
        budget,
        &mut ShortcutPaths::default(),
    )
}

fn solve_with_hints(
    board: &QuantumBoard,
    exit: FlatIdx,
    target: u32,
    path: &[Dir],
    verbose: u8,
    state_limit: Option<u32>,
    budget: Duration,
    paths: &mut ShortcutPaths,
) -> BfsResult {
    let candidate = split_candidate(board);
    let deadline = candidate.map(|_| Instant::now() + budget);
    let result = forward_bfs_until(
        board,
        exit,
        target,
        path,
        &[],
        verbose,
        state_limit,
        deadline,
        if cfg!(feature = "split-shortcut-reuse") {
            Some(paths)
        } else {
            None
        },
    );
    let result = match result {
        BfsResult::Indeterminate { shortcuts, .. }
            if !deadline.is_some_and(|end| Instant::now() >= end) =>
        {
            forward_bfs_until(
                board,
                exit,
                target,
                path,
                &shortcuts,
                verbose,
                None,
                deadline,
                if cfg!(feature = "split-shortcut-reuse") {
                    Some(paths)
                } else {
                    None
                },
            )
        }
        other => other,
    };
    if !matches!(result, BfsResult::Indeterminate { .. }) {
        return result;
    }
    // Drop the interrupted solve's proofs too: they are not in a child's slot
    // space. The frontier and visited table were already freed by the solver.
    drop(result);
    let id = candidate.expect("unlimited solve without a deadline must complete");
    if verbose > 0 {
        println!("Splitting QB {id} after {:?}", budget);
    }
    split_once(board, id, |child| {
        let mut child_paths = paths.clone();
        let result = solve_with_hints(
            child,
            exit,
            target,
            path,
            verbose,
            None,
            budget,
            &mut child_paths,
        );
        // These are hints, not conditional slot proofs. Every sibling must
        // replay each route with its own fixed box/wall and directional masks.
        for path in child_paths.paths() {
            paths.remember(path.clone());
        }
        result
    })
}

fn split_once(
    board: &QuantumBoard,
    id: usize,
    mut solve_child: impl FnMut(&QuantumBoard) -> BfsResult,
) -> BfsResult {
    let qb = &board.quantum_blocks[id];
    let mut positions: Vec<_> = qb.current_positions().collect();
    positions.sort_unstable();
    let mut clean_slots = vec![0u128; board.quantum_blocks.len()];
    let mut any_clean = false;
    let mut any_shortcut = false;
    let mut publishable = false;
    for &pos in &positions {
        let mut child = board.clone();
        for &other in &positions {
            child.qb_remove_current(id as u8, other);
        }
        let is_wall = qb.wall_pos == Some(pos);
        child.set_flat(pos, if is_wall { Tile::Wall } else { Tile::Box });
        child.set_box_push_mask(pos, if is_wall { 0 } else { qb.box_push_mask_at(pos) });
        match solve_child(&child) {
            BfsResult::SomeClean {
                ruled_out,
                publishable: child_publishable,
                ..
            } => {
                any_clean = true;
                publishable |= child_publishable;
                clean_slots[id] |= 1u128 << pos;
                for (other_id, other) in child.quantum_blocks.iter().enumerate() {
                    for start in other.all_starts() {
                        if !ruled_out.contains(&(other_id, start)) {
                            clean_slots[other_id] |= 1u128 << start;
                        }
                    }
                }
            }
            BfsResult::AllShortcut => any_shortcut = true,
            BfsResult::Unreachable => {}
            BfsResult::Indeterminate { .. } => panic!("split child must be solved to completion"),
        }
    }
    if any_clean {
        let mut ruled_out = Vec::new();
        for (id, qb) in board.quantum_blocks.iter().enumerate() {
            for start in qb.all_starts() {
                if clean_slots[id] & (1u128 << start) == 0 {
                    ruled_out.push((id, start));
                }
            }
        }
        // Child shortcut proofs depend on the fixed choice; exporting them
        // unconditionally would be unsound. Omitting optional hints is safe.
        BfsResult::SomeClean {
            ruled_out,
            shortcuts: Vec::new(),
            publishable,
        }
    } else if any_shortcut {
        BfsResult::AllShortcut
    } else {
        BfsResult::Unreachable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bfs::forward_bfs;

    fn signature(result: BfsResult) -> (u8, Vec<(usize, FlatIdx)>, bool) {
        match result {
            BfsResult::SomeClean {
                mut ruled_out,
                publishable,
                ..
            } => {
                ruled_out.sort_unstable();
                (2, ruled_out, publishable)
            }
            BfsResult::AllShortcut => (1, vec![], false),
            BfsResult::Unreachable => (0, vec![], false),
            BfsResult::Indeterminate { .. } => panic!("incomplete solve"),
        }
    }

    #[test]
    fn all_single_qb_splits_preserve_full_results() {
        for fixture in [
            include_str!("../tests/fixtures/5x7-unreachable32.txt"),
            include_str!("../tests/fixtures/5x7-unreachable77.txt"),
            include_str!("../tests/fixtures/5x5-69.txt"),
        ] {
            let parsed = crate::io::parse_input(fixture).unwrap();
            for target in [
                parsed.target_steps - 1,
                parsed.target_steps,
                parsed.target_steps + 1,
            ] {
                let solve = |board: &QuantumBoard| {
                    forward_bfs(board, parsed.exit, target, &[], &[], false, 0, None)
                };
                let expected = signature(solve(&parsed.board));
                for (id, qb) in parsed.board.quantum_blocks.iter().enumerate() {
                    if qb.num_positions() < 2 {
                        continue;
                    }
                    assert_eq!(
                        signature(split_once(&parsed.board, id, solve)),
                        expected,
                        "target={target} qb={id}"
                    );
                }
                assert_eq!(
                    signature(solve_with_budget(
                        &parsed.board,
                        parsed.exit,
                        target,
                        &[],
                        0,
                        Some(0),
                        Duration::ZERO
                    )),
                    expected
                );
            }
        }
    }

    #[test]
    fn split_union_keeps_only_slots_with_a_clean_branch() {
        let parsed = crate::io::parse_input("exit 4,2\nsteps 5\n4AA0\n0Bb0\n").unwrap();
        let board = &parsed.board;
        let a = board.flat(2, 1);
        let b = board.flat(3, 1);
        let ruled = board.flat(2, 2);
        let result = split_once(board, 0, |child| {
            if child.get_flat(a) == Tile::Box {
                BfsResult::SomeClean {
                    ruled_out: vec![(1, ruled)],
                    shortcuts: vec![],
                    publishable: false,
                }
            } else {
                BfsResult::AllShortcut
            }
        });
        assert_eq!(signature(result), (2, vec![(0, b), (1, ruled)], false));
        assert_eq!(
            signature(split_once(board, 0, |_| BfsResult::Unreachable)),
            (0, vec![], false)
        );
    }

    #[test]
    fn splitting_preserves_masks_wall_choices_and_exit_publication() {
        use crate::types::{PUSH_LEFT, PUSH_UP};
        for wall in [false, true] {
            let mut parsed =
                crate::io::parse_input(include_str!("../tests/fixtures/4x4-26-directional.txt"))
                    .unwrap();
            let other = parsed.board.flat(3, 2);
            let id = parsed
                .board
                .add_quantum_block([parsed.exit, other], wall.then_some(other));
            parsed.board.quantum_blocks[id as usize].set_push_mask(parsed.exit, PUSH_UP);
            parsed.board.quantum_blocks[id as usize].set_push_mask(other, PUSH_UP | PUSH_LEFT);
            for target in [2, 26, 27] {
                let solve = |board: &QuantumBoard| {
                    forward_bfs(board, parsed.exit, target, &[], &[], false, 0, None)
                };
                let expected = signature(solve(&parsed.board));
                assert_eq!(
                    signature(split_once(&parsed.board, id as usize, |child| {
                        assert!(child.quantum_blocks[id as usize].is_empty());
                        if child.get_flat(parsed.exit) == Tile::Box {
                            assert_eq!(child.get_box_push_mask(parsed.exit), PUSH_UP);
                        } else if wall {
                            assert_eq!(child.get_flat(other), Tile::Wall);
                        } else {
                            assert_eq!(child.get_box_push_mask(other), PUSH_UP | PUSH_LEFT);
                        }
                        solve(child)
                    })),
                    expected
                );
            }
        }
    }

    #[test]
    fn small_split_families_match_unsplit() {
        use crate::types::{PUSH_ALL, PUSH_DOWN, PUSH_LEFT, PUSH_RIGHT, PUSH_UP};
        let masks = [
            PUSH_ALL,
            PUSH_UP | PUSH_DOWN,
            PUSH_LEFT | PUSH_RIGHT,
            PUSH_RIGHT,
        ];
        for walls in 0..16 {
            for variation in 0..4 {
                let mut board = QuantumBoard::new(4, 3);
                for y in 1..=3 {
                    for x in 1..=4 {
                        board.set(x, y, Tile::Floor);
                    }
                }
                board.set(1, 1, Tile::Player);
                for (i, (x, y)) in [(4, 1), (1, 3), (2, 3), (4, 3)].into_iter().enumerate() {
                    if walls & (1 << i) != 0 {
                        board.set(x, y, Tile::Wall);
                    }
                }
                let exit = board.flat(3, 2);
                for qb in 0..2 {
                    let starts = [board.flat(2 + qb, 1), board.flat(2 + qb, 2)];
                    let id = board.add_quantum_block(starts, (variation == 3).then_some(starts[1]));
                    for (i, &pos) in starts.iter().enumerate() {
                        board.quantum_blocks[id as usize]
                            .set_push_mask(pos, masks[(variation + qb as usize + i) % 4]);
                    }
                }
                for target in [3, 4, 5, 6, 8, 12, 16] {
                    let solve = |board: &QuantumBoard| {
                        forward_bfs(board, exit, target, &[], &[], false, 0, None)
                    };
                    let expected = signature(solve(&board));
                    for id in 0..2 {
                        assert_eq!(signature(split_once(&board, id, solve)), expected);
                    }
                    assert_eq!(
                        signature(solve_with_budget(
                            &board,
                            exit,
                            target,
                            &[],
                            0,
                            Some(0),
                            Duration::ZERO
                        )),
                        expected
                    );
                }
            }
        }
    }
}
