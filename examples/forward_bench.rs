use quantum::{
    bfs::{forward_bfs, BfsResult},
    io::parse_input,
    types::Dir,
};
use std::{env, fs, time::Instant};

fn main() {
    let args: Vec<_> = env::args().collect();
    assert!(
        args.len() >= 2,
        "forward_bench FILE [REPEATS] [STATE_LIMIT_OR_0] [PATH]"
    );
    let parsed = parse_input(&fs::read_to_string(&args[1]).unwrap()).unwrap();
    let repeats: usize = args.get(2).map_or(3, |s| s.parse().unwrap());
    assert!(repeats > 0);
    let limit: u32 = args.get(3).map_or(0, |s| s.parse().unwrap());
    let path: Vec<_> = args
        .get(4)
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
    let mut times = Vec::new();
    for _ in 0..repeats {
        #[cfg(feature = "profile-search")]
        quantum::search_profile::reset();
        let start = Instant::now();
        let result = forward_bfs(
            &parsed.board,
            parsed.exit,
            parsed.target_steps,
            &path,
            &[],
            false,
            0,
            (limit != 0).then_some(limit),
        );
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        #[cfg(feature = "profile-search")]
        quantum::search_profile::report();
        match result {
            BfsResult::SomeClean { mut ruled_out, .. } => {
                ruled_out.sort_unstable();
                println!("SomeClean ruled_out={ruled_out:?}");
            }
            BfsResult::Indeterminate { mut ruled_out, .. } => {
                ruled_out.sort_unstable();
                println!("Indeterminate ruled_out={ruled_out:?}");
            }
            BfsResult::AllShortcut => println!("AllShortcut"),
            BfsResult::Unreachable => println!("Unreachable"),
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "median_ms={:.3} samples_ms={times:?}",
        times[times.len() / 2]
    );
}
