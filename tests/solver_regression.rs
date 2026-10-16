use std::collections::{HashSet, VecDeque};

use quantum::bfs::{forward_bfs, BfsResult};
use quantum::board::QuantumBoard;
use quantum::types::{Dir, FlatIdx, Tile};

// Independent, single-step concrete BFS. It deliberately shares no transition,
// pruning, fingerprint, or frontier code with the solver under test.
fn reference_distance(board: &QuantumBoard, exit: FlatIdx) -> Option<u32> {
    let initial_boxes: Vec<_> = board
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(i, &tile)| (tile == Tile::Box).then_some(i as FlatIdx))
        .collect();
    let masks: Vec<_> = initial_boxes
        .iter()
        .map(|&p| board.get_box_push_mask(p))
        .collect();
    let start = (board.find_player(), initial_boxes);
    let mut seen = HashSet::from([start.clone()]);
    let mut queue = VecDeque::from([(start, 0)]);
    while let Some(((player, boxes), distance)) = queue.pop_front() {
        if player == exit {
            return Some(distance);
        }
        let (x, y) = board.coords(player);
        for dir in Dir::all() {
            let Some((nx, ny)) = dir.apply(x, y) else {
                continue;
            };
            if !board.in_inner(nx, ny) {
                continue;
            }
            let next = board.flat(nx, ny);
            if board.get_flat(next) == Tile::Wall {
                continue;
            }
            let mut next_boxes = boxes.clone();
            if let Some(i) = boxes.iter().position(|&p| p == next) {
                if masks[i] & dir.mask() == 0 {
                    continue;
                }
                let Some((bx, by)) = dir.apply(nx, ny) else {
                    continue;
                };
                if !board.in_inner(bx, by) {
                    continue;
                }
                let beyond = board.flat(bx, by);
                if board.get_flat(beyond) == Tile::Wall || boxes.contains(&beyond) {
                    continue;
                }
                next_boxes[i] = beyond;
            }
            let state = (next, next_boxes);
            if seen.insert(state.clone()) {
                queue.push_back((state, distance + 1));
            }
        }
    }
    None
}

#[test]
fn forward_search_matches_independent_small_board_solver() {
    for walls in 0..64u32 {
        for push_mask in 0..16 {
            let mut board = QuantumBoard::new(3, 3);
            let mut wall_bit = 0;
            for y in 1..=3 {
                for x in 1..=3 {
                    let tile = match (x, y) {
                        (1, 1) => Tile::Player,
                        (2, 2) => Tile::Box,
                        (3, 3) => Tile::Floor,
                        _ => {
                            let wall = walls & (1 << wall_bit) != 0;
                            wall_bit += 1;
                            if wall {
                                Tile::Wall
                            } else {
                                Tile::Floor
                            }
                        }
                    };
                    board.set(x, y, tile);
                }
            }
            board.set_box_push_mask(board.flat(2, 2), push_mask);
            let exit = board.flat(3, 3);
            match reference_distance(&board, exit) {
                Some(distance) => {
                    let result = forward_bfs(&board, exit, distance, &[], &[], false, 0, None);
                    assert!(
                        matches!(result, BfsResult::SomeClean { .. }),
                        "walls={walls}, mask={push_mask}, distance={distance}"
                    );
                    let result = forward_bfs(&board, exit, distance + 1, &[], &[], false, 0, None);
                    assert!(
                        matches!(result, BfsResult::AllShortcut),
                        "walls={walls}, mask={push_mask}"
                    );
                }
                None => {
                    let result = forward_bfs(&board, exit, 16, &[], &[], false, 0, None);
                    assert!(
                        matches!(result, BfsResult::Unreachable),
                        "walls={walls}, mask={push_mask}"
                    );
                }
            }
        }
    }
}

#[test]
fn known_quantum_records_remain_solvable_at_their_target() {
    for fixture in [
        include_str!("fixtures/5x5-69.txt"),
        include_str!("fixtures/6x5-83.txt"),
    ] {
        let parsed = quantum::io::parse_input(fixture).unwrap();
        let result = forward_bfs(
            &parsed.board,
            parsed.exit,
            parsed.target_steps,
            &[],
            &[],
            false,
            0,
            None,
        );
        assert!(
            matches!(result, BfsResult::SomeClean { .. }),
            "target={}",
            parsed.target_steps
        );
    }
}

#[test]
fn four_by_four_record_assignments_match_reference() {
    use quantum::types::{PUSH_DOWN, PUSH_LEFT, PUSH_RIGHT, PUSH_UP};
    for (box_x, mask) in [(2, PUSH_UP), (3, PUSH_UP | PUSH_LEFT)] {
        let mut board = QuantumBoard::new(4, 4);
        for (y, row) in ["0001", "0004", "0221", "0020"].iter().enumerate() {
            for (x, cell) in row.chars().enumerate() {
                board.set(
                    x as u16 + 1,
                    y as u16 + 1,
                    match cell {
                        '1' => Tile::Wall,
                        '2' => Tile::Box,
                        '4' => Tile::Player,
                        _ => Tile::Floor,
                    },
                );
            }
        }
        board.set(box_x, 2, Tile::Box);
        for (x, y, push_mask) in [
            (box_x, 2, mask),
            (2, 3, PUSH_RIGHT),
            (3, 3, PUSH_DOWN),
            (3, 4, PUSH_RIGHT),
        ] {
            board.set_box_push_mask(board.flat(x, y), push_mask);
        }
        let exit = board.flat(2, 2);
        let distance = reference_distance(&board, exit).unwrap();
        assert_eq!(distance, if box_x == 2 { 26 } else { 2 });
        assert!(matches!(
            forward_bfs(&board, exit, distance, &[], &[], false, 0, None),
            BfsResult::SomeClean { .. }
        ));
        assert!(matches!(
            forward_bfs(&board, exit, distance + 1, &[], &[], false, 0, None),
            BfsResult::AllShortcut
        ));
    }
}

#[test]
fn directional_record_fixture_matches_reference() {
    let mut parsed =
        quantum::io::parse_input(include_str!("fixtures/4x4-26-directional.txt")).unwrap();
    assert_eq!(reference_distance(&parsed.board, parsed.exit), Some(26));
    let boxes: Vec<_> = parsed
        .board
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(i, &tile)| (tile == Tile::Box).then_some(i as FlatIdx))
        .collect();
    for pos in boxes {
        parsed
            .board
            .set_box_push_mask(pos, quantum::types::PUSH_ALL);
    }
    assert_eq!(reference_distance(&parsed.board, parsed.exit), Some(2));
}

#[test]
fn record_requires_a_clean_assignment_with_initially_uncovered_exit() {
    use quantum::types::{PUSH_LEFT, PUSH_UP};
    let mut parsed =
        quantum::io::parse_input(include_str!("fixtures/4x4-26-directional.txt")).unwrap();
    assert!(matches!(
        forward_bfs(&parsed.board, parsed.exit, 26, &[], &[], false, 0, None),
        BfsResult::SomeClean {
            publishable: false,
            ..
        }
    ));
    let other = parsed.board.flat(3, 2);
    let id = parsed.board.add_quantum_block([parsed.exit, other], None);
    parsed.board.quantum_blocks[id as usize].set_push_mask(parsed.exit, PUSH_UP);
    parsed.board.quantum_blocks[id as usize].set_push_mask(other, PUSH_UP | PUSH_LEFT);
    // The alternative leaves the exit clear, but has a two-step shortcut.
    assert!(matches!(
        forward_bfs(&parsed.board, parsed.exit, 26, &[], &[], false, 0, None),
        BfsResult::SomeClean {
            publishable: false,
            ..
        }
    ));
    assert!(matches!(
        forward_bfs(&parsed.board, parsed.exit, 2, &[], &[], false, 0, None),
        BfsResult::SomeClean {
            publishable: true,
            ..
        }
    ));

    // Push onto the exit, then off it. Only the initial position is restricted.
    let corridor = quantum::io::parse_input("exit 3,1\nsteps 2\n4200\n").unwrap();
    assert_eq!(reference_distance(&corridor.board, corridor.exit), Some(2));
    assert!(matches!(
        forward_bfs(&corridor.board, corridor.exit, 2, &[], &[], false, 0, None),
        BfsResult::SomeClean {
            publishable: true,
            ..
        }
    ));
}

#[test]
fn queued_arrivals_allow_a_later_cheaper_path() {
    use quantum::bfs::{node_fingerprint, BfsNode, SimpleVisited, VisitedTracker};
    let mut visited = SimpleVisited::new();
    let mut child = BfsNode {
        player: 7,
        steps: 9,
        qstate: quantum::qstate::QState::new(&[]),
        box_state: quantum::deadlock::BoxState::default(),
        witness_idx: None,
    };
    let fp = node_fingerprint(&child);
    assert!(visited.record_parent(&child, 1, &[]));
    assert!(
        !visited.contains(fp),
        "queued is not settled for imputation"
    );
    assert!(!visited.record_parent(&child, 1, &[]));
    child.steps = 5;
    assert!(visited.record_parent(&child, 2, &[]));
    assert!(!visited.insert(fp, 9));
    assert!(visited.insert(fp, 5));
    assert!(visited.contains(fp));
    assert!(!visited.record_parent(&child, 2, &[]));
    assert_eq!(visited.len(), 1);
}

#[test]
fn fixed_wall_distance_accounts_for_detours_but_not_boxes() {
    let parsed = quantum::io::parse_input("exit 3,1\nsteps 6\n410\n010\n020\n").unwrap();
    let distances = quantum::bfs::wall_distances(&parsed.board, parsed.exit);
    assert_eq!(distances[parsed.board.find_player() as usize], 6);
    assert_eq!(distances[parsed.board.flat(2, 1) as usize], u8::MAX);
}

#[test]
fn slow_quantum_fixtures_preserve_results() {
    for fixture in [
        include_str!("fixtures/6x5-slow45.txt"),
        include_str!("fixtures/6x5-slow49.txt"),
    ] {
        let parsed = quantum::io::parse_input(fixture).unwrap();
        match forward_bfs(
            &parsed.board,
            parsed.exit,
            parsed.target_steps,
            &[],
            &[],
            false,
            0,
            None,
        ) {
            BfsResult::SomeClean { mut ruled_out, .. } => {
                ruled_out.sort_unstable();
                assert_eq!(ruled_out, vec![(3, 37), (4, 45)]);
            }
            _ => panic!("slow fixture result changed"),
        }
    }
    let parsed = quantum::io::parse_input(include_str!("fixtures/6x5-slow45-edge.txt")).unwrap();
    assert!(matches!(
        forward_bfs(
            &parsed.board,
            parsed.exit,
            parsed.target_steps,
            &[],
            &[],
            false,
            0,
            None
        ),
        BfsResult::AllShortcut
    ));
}

#[test]
fn two_quantum_blocks_match_all_concrete_assignments() {
    use quantum::types::{PUSH_ALL, PUSH_DOWN, PUSH_LEFT, PUSH_RIGHT, PUSH_UP};
    for walls in 0..16 {
        for variation in 0..4 {
            let mut base = QuantumBoard::new(4, 3);
            for y in 1..=3 {
                for x in 1..=4 {
                    base.set(x, y, Tile::Floor);
                }
            }
            base.set(1, 1, Tile::Player);
            for (i, (x, y)) in [(4, 1), (1, 3), (2, 3), (4, 3)].into_iter().enumerate() {
                if walls & (1 << i) != 0 {
                    base.set(x, y, Tile::Wall);
                }
            }
            let starts = [
                [base.flat(2, 1), base.flat(2, 2)],
                [base.flat(3, 1), base.flat(3, 2)],
            ];
            let masks = [
                PUSH_ALL,
                PUSH_UP | PUSH_DOWN,
                PUSH_LEFT | PUSH_RIGHT,
                PUSH_RIGHT,
            ];
            let exit = base.flat(3, 2);
            let mut quantum = base.clone();
            for qb in 0..2 {
                let id = quantum.add_quantum_block(starts[qb], None);
                for i in 0..2 {
                    quantum.quantum_blocks[id as usize]
                        .set_push_mask(starts[qb][i], masks[(variation + qb + i) % 4]);
                }
            }
            let mut distances = [[None; 2]; 2];
            let mut targets = std::collections::BTreeSet::new();
            targets.insert(16);
            for a in 0..2 {
                for b in 0..2 {
                    let mut concrete = base.clone();
                    for (qb, choice) in [(0, a), (1, b)] {
                        concrete.set_flat(starts[qb][choice], Tile::Box);
                        concrete.set_box_push_mask(
                            starts[qb][choice],
                            masks[(variation + qb + choice) % 4],
                        );
                    }
                    distances[a][b] = reference_distance(&concrete, exit);
                    if let Some(d) = distances[a][b] {
                        targets.insert(d);
                        targets.insert(d + 1);
                    }
                }
            }
            for target in targets {
                let exact = distances.iter().flatten().any(|&d| d == Some(target));
                let shortcut = distances
                    .iter()
                    .flatten()
                    .any(|&d| d.map_or(false, |d| d < target));
                match forward_bfs(&quantum, exit, target, &[], &[], false, 0, None) {
                    BfsResult::SomeClean {
                        ruled_out,
                        publishable,
                        ..
                    } => {
                        assert!(exact, "walls={walls} variation={variation} target={target}");
                        assert_eq!(publishable, (0..2).any(|a| distances[a][0] == Some(target)));
                        for qb in 0..2 {
                            for choice in 0..2 {
                                let clean = (0..2).any(|other| {
                                    if qb == 0 {
                                        distances[choice][other] == Some(target)
                                    } else {
                                        distances[other][choice] == Some(target)
                                    }
                                });
                                assert_eq!(ruled_out.contains(&(qb, starts[qb][choice])), !clean);
                            }
                        }
                    }
                    BfsResult::AllShortcut => assert!(!exact && shortcut),
                    BfsResult::Unreachable => assert!(!exact && !shortcut),
                    BfsResult::Indeterminate { .. } => panic!("unlimited solve was incomplete"),
                }
            }
        }
    }
}
