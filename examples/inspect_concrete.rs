//! Diagnostic only: enumerate root QB assignments and use the independent
//! single-step concrete transition rules from tests/solver_regression.rs.
use quantum::{
    board::QuantumBoard,
    io::{format_board, parse_input},
    types::{Dir, FlatIdx, Tile},
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::{
    env, fs,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

type Key = SmallVec<[u8; 16]>;
struct Arrival {
    key: Key,
    parent: usize,
    dir: Dir,
    depth: u32,
}
struct Answer {
    path: Option<Vec<Dir>>,
    seen: usize,
    limited: bool,
}

fn unlabel(text: &str) -> String {
    text.lines()
        .filter(|line| *line != "Letters:" && *line != "Push masks:")
        .collect::<Vec<_>>()
        .join("\n")
}

fn oracle(board: &QuantumBoard, exit: FlatIdx, target: u32) -> quantum::bfsrelaxed::RelaxedResult {
    use quantum::{
        bfs::{wall_distances, BfsNode, SearchContext},
        bfsrelaxed::{relaxed_solve, RelaxedCache},
        deadlock::BoxState,
        qstate::QState,
    };
    assert!(board.quantum_blocks.is_empty());
    let mut walls = 0u128;
    let mut boxes = 0u128;
    let mut inner = 0u128;
    let mut pushable = [0u128; 4];
    for p in 0..board.width * board.height {
        let bit = 1u128 << p;
        let (x, y) = board.coords(p);
        if board.in_inner(x, y) {
            inner |= bit;
        }
        match board.get_flat(p) {
            Tile::Wall => walls |= bit,
            Tile::Box => {
                boxes |= bit;
                for dir in Dir::all() {
                    if board.get_box_push_mask(p) & dir.mask() != 0 {
                        pushable[dir.idx()] |= bit;
                    }
                }
            }
            _ => {}
        }
    }
    let mut ctx = SearchContext {
        q_occupancy: 0,
        box_occupancy: boxes,
        covered_mask: walls | boxes,
        static_walls: walls,
        static_boxes: boxes,
        static_inner_mask: inner,
        box_origin_groups: BoxState::origin_groups(board),
        pushable,
        relaxed_cache: RelaxedCache::new(),
        exit_distances: wall_distances(board, exit),
        #[cfg(feature = "walking-dominance")]
        walks: Default::default(),
    };
    let root = BfsNode {
        player: board.find_player(),
        steps: 0,
        qstate: QState::new(&[]),
        box_state: BoxState::default(),
        witness_idx: None,
    };
    relaxed_solve(
        board,
        &[],
        root.player,
        exit,
        target,
        0,
        None,
        &root,
        &mut ctx,
    )
}

fn solve(board: &QuantumBoard, exit: FlatIdx, cap: u32, state_cap: usize) -> Answer {
    let boxes: Vec<_> = board
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(p, &t)| (t == Tile::Box).then_some(p as FlatIdx))
        .collect();
    let masks: Vec<_> = boxes.iter().map(|&p| board.get_box_push_mask(p)).collect();
    let mut root = Key::new();
    root.push(board.find_player() as u8);
    root.extend(boxes.iter().map(|&p| p as u8));
    let mut seen = FxHashMap::default();
    seen.insert(root.clone(), 0usize);
    let mut arena = vec![Arrival {
        key: root,
        parent: 0,
        dir: Dir::Up,
        depth: 0,
    }];
    let (ex, ey) = board.coords(exit);
    let mut head = 0;
    while head < arena.len() {
        let key = arena[head].key.clone();
        let depth = arena[head].depth;
        if key[0] as FlatIdx == exit {
            let mut path = Vec::new();
            let mut at = head;
            while at != 0 {
                path.push(arena[at].dir);
                at = arena[at].parent;
            }
            path.reverse();
            return Answer {
                path: Some(path),
                seen: seen.len(),
                limited: false,
            };
        }
        let (x, y) = board.coords(key[0] as FlatIdx);
        if depth + u32::from(x.abs_diff(ex) + y.abs_diff(ey)) <= cap && depth < cap {
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
                let mut child = key.clone();
                if let Some(i) = key[1..].iter().position(|&p| p as FlatIdx == next) {
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
                    if board.get_flat(beyond) == Tile::Wall || key[1..].contains(&(beyond as u8)) {
                        continue;
                    }
                    child[i + 1] = beyond as u8;
                }
                child[0] = next as u8;
                if seen.contains_key(&child) {
                    continue;
                }
                if seen.len() >= state_cap {
                    return Answer {
                        path: None,
                        seen: seen.len(),
                        limited: true,
                    };
                }
                seen.insert(child.clone(), arena.len());
                arena.push(Arrival {
                    key: child,
                    parent: head,
                    dir,
                    depth: depth + 1,
                });
            }
        }
        head += 1;
    }
    Answer {
        path: None,
        seen: seen.len(),
        limited: false,
    }
}

fn moves(path: &[Dir]) -> String {
    path.iter()
        .map(|d| match d {
            Dir::Up => 'U',
            Dir::Down => 'D',
            Dir::Left => 'L',
            Dir::Right => 'R',
        })
        .collect()
}

fn push_trace(board: &QuantumBoard, path: &[Dir]) -> (usize, String) {
    let mut boxes: Vec<_> = board
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(p, &t)| (t == Tile::Box).then_some((p as FlatIdx, p as FlatIdx)))
        .collect();
    let mut player = board.find_player();
    let mut result = String::new();
    let mut pushes = 0;
    for (step, &dir) in path.iter().enumerate() {
        let (x, y) = board.coords(player);
        let (nx, ny) = dir.apply(x, y).unwrap();
        let next = board.flat(nx, ny);
        assert!(board.in_inner(nx, ny) && board.get_flat(next) != Tile::Wall);
        if let Some(i) = boxes.iter().position(|&(_, p)| p == next) {
            let (bx, by) = dir.apply(nx, ny).unwrap();
            let beyond = board.flat(bx, by);
            assert!(board.in_inner(bx, by) && board.get_flat(beyond) != Tile::Wall);
            assert!(!boxes.iter().any(|&(_, p)| p == beyond));
            assert!(board.get_box_push_mask(boxes[i].0) & dir.mask() != 0);
            let (ox, oy) = board.coords(boxes[i].0);
            result.push_str(&format!(
                "step {}: box originally ({ox},{oy}) from ({nx},{ny}) to ({bx},{by})\n",
                step + 1
            ));
            boxes[i].1 = beyond;
            pushes += 1;
        }
        player = next;
    }
    (pushes, result)
}

fn main() {
    let args: Vec<_> = env::args().collect();
    assert!(args.len() >= 3, "inspect_concrete FILE OUTPUT_DIRECTORY [STATE_CAP] [MAX_ASSIGNMENTS] [--classify PATH | --ablate | --oracle]");
    let parsed = parse_input(&unlabel(&fs::read_to_string(&args[1]).unwrap())).unwrap();
    let output = Path::new(&args[2]);
    fs::create_dir_all(output).unwrap();
    let state_cap = args.get(3).map_or(200_000, |s| s.parse().unwrap());
    let max_assignments = args.get(4).map_or(usize::MAX, |s| s.parse().unwrap());
    if args.iter().any(|arg| arg == "--oracle") {
        let answer = solve(&parsed.board, parsed.exit, parsed.target_steps, state_cap);
        println!(
            "reference_distance={:?} states={} limited={} relaxed={:?}",
            answer.path.as_ref().map(Vec::len),
            answer.seen,
            answer.limited,
            oracle(&parsed.board, parsed.exit, parsed.target_steps)
        );
        return;
    }
    let count = parsed
        .board
        .quantum_blocks
        .iter()
        .try_fold(1usize, |count, qb| count.checked_mul(qb.num_positions()));
    assert!(
        count.is_some_and(|count| count <= 100_000),
        "diagnostic assignment count exceeds 100,000; use a smaller projection"
    );
    let mut assignments = vec![(parsed.board.clone(), Vec::<FlatIdx>::new())];
    for qb in &parsed.board.quantum_blocks {
        assert!(qb.cells.iter().all(|(p, s)| s.len() == 1 && s[0] == *p));
        let mut positions: Vec<_> = qb.current_positions().collect();
        positions.sort_unstable();
        let mut next = Vec::new();
        for (board, choices) in assignments {
            for &pos in &positions {
                let mut concrete = board.clone();
                for &p in &positions {
                    concrete.set_flat(p, Tile::Floor);
                }
                let wall = qb.wall_pos == Some(pos);
                concrete.set_flat(pos, if wall { Tile::Wall } else { Tile::Box });
                concrete.set_box_push_mask(pos, if wall { 0 } else { qb.box_push_mask_at(pos) });
                let mut choices = choices.clone();
                choices.push(pos);
                next.push((concrete, choices));
            }
        }
        assignments = next;
    }
    println!("assignments={} state_cap={state_cap}", assignments.len());
    if let Some(at) = args.iter().position(|arg| arg == "--classify") {
        let intended: Vec<_> = args[at + 1]
            .chars()
            .map(|c| match c {
                'U' => Dir::Up,
                'D' => Dir::Down,
                'L' => Dir::Left,
                'R' => Dir::Right,
                _ => panic!("bad move"),
            })
            .collect();
        let mut rows =
            BufWriter::new(fs::File::create(output.join("classifications.tsv")).unwrap());
        writeln!(rows, "id\tchoices\tclass\tmilliseconds").unwrap();
        let total = Instant::now();
        let mut counts = [0usize; 3];
        for (id, (mut board, choices)) in assignments.into_iter().enumerate().take(max_assignments)
        {
            board.quantum_blocks.clear();
            push_trace(&board, &intended);
            let now = Instant::now();
            let result = quantum::bfs::forward_bfs(
                &board,
                parsed.exit,
                parsed.target_steps,
                &intended,
                &[],
                false,
                0,
                None,
            );
            let (kind, index) = match result {
                quantum::bfs::BfsResult::SomeClean { .. } => ("CLEAN", 0),
                quantum::bfs::BfsResult::AllShortcut => ("SHORTCUT", 1),
                quantum::bfs::BfsResult::Unreachable => ("UNREACHABLE", 2),
                _ => panic!("unlimited concrete solve interrupted"),
            };
            counts[index] += 1;
            let choices = choices
                .iter()
                .map(|&p| {
                    let (x, y) = board.coords(p);
                    format!("{x},{y}")
                })
                .collect::<Vec<_>>()
                .join(";");
            writeln!(
                rows,
                "{id}\t{choices}\t{kind}\t{:.3}",
                now.elapsed().as_secs_f64() * 1000.0
            )
            .unwrap();
            if id % 32 == 0 {
                rows.flush().unwrap();
                println!(
                    "id={id} seconds={:.3} clean/shortcut/unreachable={counts:?}",
                    total.elapsed().as_secs_f64()
                );
            }
        }
        println!(
            "seconds={:.3} clean/shortcut/unreachable={counts:?}",
            total.elapsed().as_secs_f64()
        );
        return;
    }
    if args.iter().any(|arg| arg == "--ablate") {
        assert!(parsed.board.quantum_blocks.is_empty());
        for (p, &tile) in parsed.board.tiles.iter().enumerate() {
            if tile != Tile::Box
                && (tile != Tile::Wall
                    || !parsed.board.in_inner(
                        parsed.board.coords(p as FlatIdx).0,
                        parsed.board.coords(p as FlatIdx).1,
                    ))
            {
                continue;
            }
            let mut board = parsed.board.clone();
            board.set_flat(p as FlatIdx, Tile::Floor);
            let started = Instant::now();
            let answer = solve(&board, parsed.exit, parsed.target_steps, state_cap);
            let (x, y) = board.coords(p as FlatIdx);
            println!("delete=({x},{y}) tile={tile:?} distance={:?} limited={} states={} milliseconds={:.3}",answer.path.as_ref().map(Vec::len),answer.limited,answer.seen,started.elapsed().as_secs_f64()*1000.0);
            if let Some(path) = answer.path {
                fs::write(
                    output.join(format!("without-{x}-{y}-route.txt")),
                    format!("{}\n{}", moves(&path), push_trace(&board, &path).1),
                )
                .unwrap();
            }
        }
        return;
    }
    let mut rows = BufWriter::new(fs::File::create(output.join("assignments.tsv")).unwrap());
    writeln!(
        rows,
        "id\tchoices\tdistance\tstates\tmilliseconds\tpushes\tconcrete_only_bound\tpath"
    )
    .unwrap();
    let mut projection_cache = FxHashMap::<u128, (Option<usize>, usize)>::default();
    let mut histogram = std::collections::BTreeMap::<usize, usize>::new();
    let mut samples = std::collections::BTreeSet::new();
    let total = Instant::now();
    let mut limited = 0;
    for (id, (mut board, choices)) in assignments.into_iter().enumerate().take(max_assignments) {
        board.quantum_blocks.clear();
        let wall_mask = board.tiles.iter().enumerate().fold(0u128, |bits, (p, &t)| {
            bits | if t == Tile::Wall { 1u128 << p } else { 0 }
        });
        let projection = projection_cache.entry(wall_mask).or_insert_with(|| {
            let mut core = board.clone();
            for &pos in &choices {
                if core.get_flat(pos) == Tile::Box {
                    core.set_flat(pos, Tile::Floor);
                }
            }
            let answer = solve(&core, parsed.exit, parsed.target_steps, state_cap);
            assert!(!answer.limited, "projection exceeded budget");
            let distance = answer.path.as_ref().map(Vec::len);
            let tag = format!("core-{:032x}", wall_mask);
            fs::write(
                output.join(format!("{tag}.txt")),
                format!(
                    "exit {},{}\nsteps {}\n{}",
                    board.coords(parsed.exit).0,
                    board.coords(parsed.exit).1,
                    parsed.target_steps,
                    format_board(&core, parsed.exit)
                ),
            )
            .unwrap();
            if let Some(path) = answer.path {
                fs::write(
                    output.join(format!("{tag}-route.txt")),
                    format!("{}\n{}", moves(&path), push_trace(&core, &path).1),
                )
                .unwrap();
            }
            (distance, answer.seen)
        });
        let started = Instant::now();
        let answer = solve(&board, parsed.exit, parsed.target_steps, state_cap);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let choice_text = choices
            .iter()
            .map(|&p| {
                let (x, y) = board.coords(p);
                format!("{x},{y}")
            })
            .collect::<Vec<_>>()
            .join(";");
        let (distance, pushes, path) = if let Some(path) = answer.path {
            let n = path.len();
            *histogram.entry(n).or_default() += 1;
            let (pushes, trace) = push_trace(&board, &path);
            if samples.insert((n, wall_mask)) {
                fs::write(
                    output.join(format!("sample-{id}-distance-{n}.txt")),
                    format!(
                        "exit {},{}\nsteps {n}\n{}",
                        board.coords(parsed.exit).0,
                        board.coords(parsed.exit).1,
                        format_board(&board, parsed.exit)
                    ),
                )
                .unwrap();
                fs::write(
                    output.join(format!("sample-{id}-route.txt")),
                    format!("choices={choice_text}\n{}\n{trace}", moves(&path)),
                )
                .unwrap();
            }
            (n.to_string(), pushes, moves(&path))
        } else {
            limited += usize::from(answer.limited);
            (
                if answer.limited {
                    "LIMIT"
                } else {
                    "BEYOND_CAP"
                }
                .to_owned(),
                0,
                String::new(),
            )
        };
        writeln!(
            rows,
            "{id}\t{choice_text}\t{distance}\t{}\t{ms:.3}\t{pushes}\t{:?}\t{path}",
            answer.seen, projection.0
        )
        .unwrap();
        if id % 32 == 0 {
            rows.flush().unwrap();
            println!(
                "id={id} elapsed={:.1}s histogram={histogram:?} limited={limited}",
                total.elapsed().as_secs_f64()
            );
        }
    }
    rows.flush().unwrap();
    println!(
        "elapsed={:.3}s histogram={histogram:?} limited={limited} cores={projection_cache:?}",
        total.elapsed().as_secs_f64()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_distance_and_route_respect_masks() {
        let mut parsed = parse_input("exit 3,1\nsteps 2\n4200\n").unwrap();
        let answer = solve(&parsed.board, parsed.exit, 2, 1000);
        let path = answer.path.unwrap();
        assert_eq!(moves(&path), "RR");
        assert_eq!(push_trace(&parsed.board, &path).0, 2);
        parsed
            .board
            .set_box_push_mask(parsed.board.flat(2, 1), Dir::Left.mask());
        let answer = solve(&parsed.board, parsed.exit, 100, 1000);
        assert!(answer.path.is_none() && !answer.limited);
    }

    #[test]
    fn budget_exhaustion_is_not_a_proof_and_reports_round_trip() {
        let parsed = parse_input("exit 3,1\nsteps 2\n400\n").unwrap();
        let answer = solve(&parsed.board, parsed.exit, 2, 1);
        assert!(answer.path.is_none() && answer.limited);
        let text = format!(
            "exit 3,1\nsteps 2\n{}",
            format_board(&parsed.board, parsed.exit)
        );
        let reread = parse_input(&unlabel(&text)).unwrap();
        assert_eq!(
            solve(&reread.board, reread.exit, 2, 1000)
                .path
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn top_row_projection_is_deadlocked_even_without_other_obstacles() {
        let mut board = QuantumBoard::new(5, 7);
        for y in 1..=7 {
            for x in 1..=5 {
                board.set(x, y, Tile::Floor);
            }
        }
        board.set(2, 2, Tile::Player);
        for (x, y, mask) in [(2, 1, 9), (4, 1, 8), (3, 2, 8), (4, 2, 1), (3, 3, 2)] {
            board.set(x, y, Tile::Box);
            board.set_box_push_mask(board.flat(x, y), mask);
        }
        let answer = solve(&board, board.flat(3, 3), 10_000, 1000);
        assert!(answer.path.is_none() && !answer.limited);
        assert_eq!(answer.seen, 59);
    }
}
