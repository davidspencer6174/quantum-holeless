use quantum::{
    bfs::{forward_bfs, forward_bfs_complete, BfsResult},
    io::parse_input,
    types::{Dir, Tile},
};
use std::{env, fs, time::Instant};

fn main() {
    let args: Vec<_> = env::args().collect();
    assert!(
        args.len() >= 2,
        "qb_split_bench FILE [PATH] [QB_INDEX] [adaptive]"
    );
    let parsed = parse_input(&fs::read_to_string(&args[1]).unwrap()).unwrap();
    let path: Vec<_> = args
        .get(2)
        .map_or("", String::as_str)
        .chars()
        .map(|c| match c {
            'U' => Dir::Up,
            'D' => Dir::Down,
            'L' => Dir::Left,
            'R' => Dir::Right,
            _ => panic!("invalid path character"),
        })
        .collect();
    let only: Option<usize> = args.get(3).map(|s| s.parse().unwrap());
    let adaptive = args.get(4).is_some_and(|s| s == "adaptive");
    for (id, qb) in parsed.board.quantum_blocks.iter().enumerate() {
        if only.is_some_and(|only| only != id) || qb.num_positions() < 2 {
            continue;
        }
        let mut positions: Vec<_> = qb.current_positions().collect();
        positions.sort_unstable();
        let start = Instant::now();
        for &pos in &positions {
            // This harness accepts root boards, where each slot starts at its
            // current cell. Preserve the remaining QBs' original IDs.
            assert!(qb
                .cells
                .iter()
                .all(|(p, starts)| starts.len() == 1 && starts[0] == *p));
            let mut board = parsed.board.clone();
            for &p in &positions {
                board.qb_remove_current(id as u8, p);
            }
            let is_wall = qb.wall_pos == Some(pos);
            board.set_flat(pos, if is_wall { Tile::Wall } else { Tile::Box });
            board.set_box_push_mask(pos, if is_wall { 0 } else { qb.box_push_mask_at(pos) });
            let branch_start = Instant::now();
            let result = if adaptive {
                forward_bfs_complete(
                    &board,
                    parsed.exit,
                    parsed.target_steps,
                    &path,
                    0,
                    Some(10_000),
                )
            } else {
                forward_bfs(
                    &board,
                    parsed.exit,
                    parsed.target_steps,
                    &path,
                    &[],
                    false,
                    0,
                    None,
                )
            };
            let result = match result {
                BfsResult::SomeClean {
                    mut ruled_out,
                    publishable,
                    ..
                } => {
                    ruled_out.sort_unstable();
                    format!("SomeClean publishable={publishable} ruled_out={ruled_out:?}")
                }
                BfsResult::AllShortcut => "AllShortcut".to_owned(),
                BfsResult::Unreachable => "Unreachable".to_owned(),
                BfsResult::Indeterminate { .. } => panic!("unlimited solve interrupted"),
            };
            println!(
                "qb={id} position={pos} ms={:.3} {result}",
                branch_start.elapsed().as_secs_f64() * 1000.0
            );
        }
        println!(
            "qb={id} branches={} total_ms={:.3}",
            positions.len(),
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
