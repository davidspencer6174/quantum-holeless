use quantum::backward::{backward_dfs_with_options, BackwardSearchOptions};
use std::{fs, path::PathBuf};

fn test_directory() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scratch/tests-workers")
        .join(format!("{}-{stamp}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn dfs_order(moves: &str) -> Vec<u8> {
    moves
        .bytes()
        .map(|c| match c {
            b'U' => 0,
            b'D' => 1,
            b'L' => 2,
            b'R' => 3,
            _ => panic!("invalid move"),
        })
        .collect()
}

#[test]
fn parallel_search_preserves_record_and_orders_each_workers_subtrees() {
    let root = test_directory();
    for (threads, split_depth) in [(1, 10), (2, 3), (4, 10), (4, 15)] {
        let directory = root.join(format!("threads-{threads}-depth-{split_depth}"));
        let options = BackwardSearchOptions {
            threads,
            split_depth,
            log_dir: directory.join("logs"),
            record_dir: directory,
            ..Default::default()
        };
        let summary = backward_dfs_with_options(2, 2, 4, 4, &options).unwrap();
        assert_eq!(summary.longest_found, 22);
        let records =
            fs::read_to_string(options.record_dir.join("longest_levels_4_4.txt")).unwrap();
        let lines: Vec<_> = records.lines().collect();
        let mut depths = Vec::new();
        for (i, line) in lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.starts_with("=== Longest"))
        {
            let depth: usize = line
                .split("depth = ")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            depths.push(depth);
            assert_eq!(lines[i + 1], "Letters:");
            assert!(lines[i + 2..i + 6].iter().all(|line| line.len() == 4));
            assert_eq!(lines[i + 7], "Push masks:");
            assert!(lines[i + 8..i + 12]
                .iter()
                .all(|line| line.split_whitespace().count() == 4));
        }
        assert_eq!(depths.last(), Some(&22));
        assert!(
            depths.windows(2).all(|pair| pair[0] < pair[1]),
            "record writes must be ordered"
        );
        if threads == 1 {
            assert!(summary.log_directory.is_none());
            continue;
        }
        let logs = summary.log_directory.unwrap();
        assert!(logs.join("coordinator.log").is_file());
        let mut all_ids = Vec::new();
        for worker in 1..=threads {
            let text = fs::read_to_string(logs.join(format!("worker-{worker:02}.log"))).unwrap();
            assert!(text.contains(&format!("[worker:{worker:02}] [progress:finished]")));
            let mut previous_id = 0;
            let mut previous_prefix = Vec::new();
            let mut started = None;
            for line in text.lines() {
                if line.starts_with("[subtree:start]") {
                    assert!(started.is_none(), "each worker owns one subtree at a time");
                    let id: usize = line
                        .split("id=")
                        .nth(1)
                        .unwrap()
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    let prefix = dfs_order(line.split("moves=").nth(1).unwrap());
                    assert!(id > previous_id);
                    assert!(prefix > previous_prefix);
                    assert_eq!(prefix.len(), split_depth);
                    previous_id = id;
                    previous_prefix = prefix;
                    started = Some(id);
                    all_ids.push(id);
                } else if line.starts_with("[subtree:finished]") {
                    let id: usize = line
                        .split("id=")
                        .nth(1)
                        .unwrap()
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert_eq!(started.take(), Some(id));
                }
            }
            assert!(started.is_none());
        }
        all_ids.sort_unstable();
        assert!(!all_ids.is_empty());
        assert_eq!(
            all_ids,
            (1..=summary.subtrees_completed).collect::<Vec<_>>()
        );
    }
}

#[test]
fn rejects_invalid_worker_settings_before_starting() {
    for options in [
        BackwardSearchOptions {
            threads: 0,
            ..Default::default()
        },
        BackwardSearchOptions {
            split_depth: 0,
            ..Default::default()
        },
    ] {
        assert_eq!(
            backward_dfs_with_options(2, 2, 4, 4, &options)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn completed_checkpoints_resume_with_changed_worker_count_without_duplicate_records() {
    let directory = test_directory();
    let mut options = BackwardSearchOptions {
        threads: 2,
        split_depth: 3,
        checkpoint_dir: Some(directory.join("checkpoint")),
        log_dir: directory.join("logs"),
        record_dir: directory.join("unused-records"),
        ..Default::default()
    };
    let first = backward_dfs_with_options(2, 2, 4, 4, &options).unwrap();
    assert_eq!(first.longest_found, 22);
    assert!(first.subtrees_completed > 0);
    let record_path = directory.join("checkpoint/longest_levels_4_4.txt");
    let records = fs::read(&record_path).unwrap();
    let snapshots: std::collections::BTreeMap<_, _> =
        fs::read_dir(directory.join("checkpoint/search-records/subtrees"))
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let text = fs::read_to_string(&path).unwrap();
                (path, text)
            })
            .collect();
    assert_eq!(snapshots.len(), first.subtrees_completed);
    assert!(snapshots
        .values()
        .any(|text| text.lines().nth(1).unwrap().split('\t').nth(3) == Some("22")));
    for text in snapshots.values() {
        let row: Vec<_> = text.lines().nth(1).unwrap().split('\t').collect();
        assert_eq!(row[2], "finished");
        if row[3] == "0" {
            continue;
        }
        let input = text.split_once("Input:\n").unwrap().1;
        let parsed = quantum::io::parse_input(input).unwrap();
        assert!(parsed.board.quantum_blocks.is_empty());
        assert!(matches!(
            quantum::bfs::forward_bfs(
                &parsed.board,
                parsed.exit,
                parsed.target_steps,
                &[],
                &[],
                false,
                0,
                None
            ),
            quantum::bfs::BfsResult::SomeClean {
                publishable: true,
                ..
            }
        ));
    }
    for name in [
        "most-pushes.txt",
        "most-push-edges.txt",
        "most-box-directions.txt",
    ] {
        let text =
            fs::read_to_string(directory.join("checkpoint/search-records").join(name)).unwrap();
        assert!(text.contains("witness=concrete"));
        let parsed = quantum::io::parse_input(text.split_once("Input:\n").unwrap().1).unwrap();
        assert!(matches!(
            quantum::bfs::forward_bfs(
                &parsed.board,
                parsed.exit,
                parsed.target_steps,
                &[],
                &[],
                false,
                0,
                None
            ),
            quantum::bfs::BfsResult::SomeClean {
                publishable: true,
                ..
            }
        ));
    }
    options.resume = true;
    options.threads = 1;
    let second = backward_dfs_with_options(2, 2, 4, 4, &options).unwrap();
    assert_eq!(second.longest_found, 22);
    assert_eq!(second.subtrees_completed, 0);
    assert_eq!(second.checkpoint_skips, first.subtrees_completed);
    let ids = |directory: &std::path::Path| {
        let mut result = std::collections::BTreeMap::new();
        for file in fs::read_dir(directory).unwrap() {
            let file = file.unwrap().path();
            for line in fs::read_to_string(file).unwrap().lines() {
                if line.starts_with("[subtree:start]") || line.starts_with("[subtree:saved]") {
                    let id: usize = line
                        .split("id=")
                        .nth(1)
                        .unwrap()
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    result.insert(id, line.split("moves=").nth(1).unwrap().to_owned());
                }
            }
        }
        result
    };
    assert_eq!(
        ids(first.log_directory.as_ref().unwrap()),
        ids(second.log_directory.as_ref().unwrap())
    );
    assert_eq!(records, fs::read(record_path).unwrap());
    for (path, text) in snapshots {
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }
    options.symmetry = false;
    assert!(backward_dfs_with_options(2, 2, 4, 4, &options).is_err());
    options.symmetry = true;
    options.split_depth = 4;
    assert!(backward_dfs_with_options(2, 2, 4, 4, &options).is_err());
}

#[test]
fn symmetry_preserves_small_search_records_for_every_exit() {
    let root = test_directory();
    let mut pruned = false;
    for (w, h) in [(3, 3), (3, 4), (4, 4)] {
        for x in 1..=w {
            for y in 1..=h {
                let mut results = Vec::new();
                for symmetry in [false, true] {
                    let options = BackwardSearchOptions {
                        record_dir: root.join(format!("{w}-{h}-{x}-{y}-{symmetry}")),
                        symmetry,
                        ..Default::default()
                    };
                    results.push(backward_dfs_with_options(x, y, w, h, &options).unwrap());
                }
                assert_eq!(
                    results[0].longest_found, results[1].longest_found,
                    "{w}x{h} exit {x},{y}"
                );
                pruned |= results[1].unique_boards < results[0].unique_boards;
            }
        }
    }
    assert!(pruned);
}
