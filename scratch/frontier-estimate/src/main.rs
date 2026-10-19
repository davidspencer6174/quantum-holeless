#![allow(dead_code, unused_imports, unused_variables)]
pub use quantum::{bfs, board, io, qstate, shortcut_paths, types};
#[path = "../../../src/backward_deadlock.rs"]
mod backward_deadlock;
#[path = "../../../src/progress.rs"]
mod progress;
#[path = "../../../src/search_log.rs"]
mod search_log;
#[path = "../../../src/work_queue.rs"]
mod work_queue;
#[path = "../../../src/checkpoint.rs"]
mod checkpoint;
#[path = "../../../src/symmetry.rs"]
mod symmetry;
#[path = "../../../src/search_records.rs"]
mod search_records;

mod backward {
    include!("../../../src/backward.rs");

    pub fn frontier(width: u16, height: u16, x: u16, y: u16, split_depth: usize) -> Vec<(usize, String)> {
        let board = QuantumBoard::wall_filled_with_player(width, height, x, y);
        let exit = board.flat(x, y);
        let mut state = SharedState::new(width, height, (x, y), 1);
        state.longest_found = 110;
        let runtime = Arc::new(SearchRuntime {
            state: Mutex::new(state), queue: Some(WorkQueue::new(32)),
            split_depth, record_path: PathBuf::from("scratch/frontier-estimate/unused-records.txt"),
            checkpoint: None, frontier_seen: Some(Mutex::new(FxHashMap::default())),
            symmetries: crate::symmetry::exit_symmetries(width, height, x, y),
            stopping: AtomicBool::new(false), checkpoint_skips: std::sync::atomic::AtomicUsize::new(0),
            records: None,
        });
        let log_path = PathBuf::from(format!("scratch/frontier-estimate/prefix-scan-{}.log", std::process::id()));
        let _routing = search_log::enter(0, SearchLog::create(&log_path).unwrap());
        std::thread::scope(|scope| {
            let queue = runtime.queue.as_ref().unwrap();
            let consumer = scope.spawn(|| {
                let mut prefixes = Vec::new();
                while let Some(job) = queue.pop() {
                    prefixes.push((job.id, backward_moves(&job.path)));
                }
                prefixes
            });
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                backward_dfs_recursive(&board, 0, exit, &runtime, None, vec![], ShortcutPaths::default(), 0, None);
            }));
            if result.is_err() { queue.cancel(); } else { queue.finish(); }
            let prefixes = consumer.join().unwrap();
            if let Err(panic) = result { std::panic::resume_unwind(panic); }
            prefixes
        })
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() && args.len() != 6 {
        eprintln!("Usage: frontier-estimate [WIDTH HEIGHT EXIT_X EXIT_Y SPLIT_DEPTH OUTPUT_TSV]");
        std::process::exit(1);
    }
    let (width, height, x, y, split_depth, output_path) = if args.is_empty() {
        (5, 7, 3, 3, 10, "scratch/frontier-estimate/frontier.tsv")
    } else {
        let numbers: Vec<u16> = args[..5].iter().map(|arg| arg.parse().expect("Expected a positive integer fitting u16")).collect();
        assert!(numbers.iter().all(|&n| n > 0), "Arguments must be positive");
        assert!(numbers[2] <= numbers[0] && numbers[3] <= numbers[1], "Exit must be inside the board");
        (numbers[0], numbers[1], numbers[2], numbers[3], usize::from(numbers[4]), args[5].as_str())
    };
    let start = std::time::Instant::now();
    let prefixes = backward::frontier(width, height, x, y, split_depth);
    let output: String = prefixes.iter().map(|(id, prefix)| format!("{id}\t{prefix}\n")).collect();
    std::fs::write(output_path, output).unwrap();
    println!("prefixes={} elapsed_s={:.3}", prefixes.len(), start.elapsed().as_secs_f64());
    for dir in ['U', 'D', 'L', 'R'] {
        println!("{dir}: {}", prefixes.iter().filter(|(_, p)| p.starts_with(dir)).count());
    }
    for cursor in ["UULLDDDDDR", "UURRDDDDDL", "UURRDDLDLL"] {
        let rank = prefixes.iter().position(|(_, p)| p == cursor);
        println!("cursor={cursor} index={rank:?}");
    }
}
