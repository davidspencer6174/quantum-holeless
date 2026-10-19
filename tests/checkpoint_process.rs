use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn directory() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scratch/tests-process-checkpoint")
        .join(format!("{}-{stamp}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn start(root: &Path, resume: bool) -> Child {
    Command::new(env!("CARGO_BIN_EXE_quantum"))
        .args([
            "4",
            "5",
            "2",
            "3",
            "--threads",
            if resume { "1" } else { "3" },
            "--split-depth",
            "3",
        ])
        .arg(if resume { "--resume" } else { "--checkpoint" })
        .arg(root.join("checkpoint"))
        .arg("--log-dir")
        .arg(root.join("logs"))
        .stdout(Stdio::from(
            File::create(root.join(if resume { "resume.txt" } else { "start.txt" })).unwrap(),
        ))
        .stderr(Stdio::from(
            File::create(root.join(if resume {
                "resume-errors.txt"
            } else {
                "start-errors.txt"
            }))
            .unwrap(),
        ))
        .spawn()
        .unwrap()
}

fn completion_count(path: &Path) -> usize {
    let Ok(mut file) = File::open(path) else {
        return 0;
    };
    let mut count = 0;
    loop {
        let mut size = [0; 4];
        if file.read_exact(&mut size).is_err() {
            break;
        }
        let n = u32::from_le_bytes(size) as usize;
        assert!(n < 1024 * 1024);
        let mut payload = vec![0; n];
        let mut check = [0; 8];
        if file.read_exact(&mut payload).is_err() || file.read_exact(&mut check).is_err() {
            break;
        }
        count += usize::from(payload.first() == Some(&b'D'));
    }
    count
}

fn finish(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("test search timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn killed_process_resumes_completed_subtrees_and_recovers_a_torn_tail() {
    let root = directory();
    let journal = root.join("checkpoint/checkpoint.bin");
    let mut child = start(&root, false);
    let deadline = Instant::now() + Duration::from_secs(30);
    while completion_count(&journal) < 2 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "search exited before interruption"
        );
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("no checkpoint saved");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let saved = completion_count(&journal);
    assert!(saved >= 2);
    // Trim any naturally torn tail before injecting a known incomplete frame.
    let bytes = fs::read(&journal).unwrap();
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        let n = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        if offset + n + 12 > bytes.len() {
            break;
        }
        offset += n + 12;
    }
    let file = OpenOptions::new().write(true).open(&journal).unwrap();
    file.set_len(offset as u64).unwrap();
    drop(file);
    OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(&[50, 0, 0, 0, b'D', 9])
        .unwrap();
    finish(&mut start(&root, true));
    let output = fs::read_to_string(root.join("resume.txt")).unwrap();
    assert!(output.contains("best=40"), "{output}");
    assert!(!output.contains("Checkpoint: skipped 0 saved subtrees"));
    assert!(output.contains("search complete"));
    let records = fs::read_to_string(root.join("checkpoint/longest_levels_4_5.txt")).unwrap();
    assert!(records.contains("depth = 40"));
    let depths: Vec<usize> = records
        .lines()
        .filter_map(|line| {
            line.strip_prefix("=== Longest path so far with depth = ")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .collect();
    assert!(depths.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn stop_request_finishes_active_work_and_is_consumed_on_resume() {
    let root = directory();
    let mut child = start(&root, false);
    let deadline = Instant::now() + Duration::from_secs(30);
    while completion_count(&root.join("checkpoint/checkpoint.bin")) == 0 {
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("no checkpoint saved");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let status = Command::new(env!("CARGO_BIN_EXE_quantum"))
        .arg("--stop")
        .arg(root.join("checkpoint"))
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    finish(&mut child);
    let stopped = fs::read_to_string(root.join("start.txt")).unwrap();
    assert!(stopped.contains("stopped; resume with --resume"));
    finish(&mut start(&root, true));
    assert!(!root.join("checkpoint/STOP").exists());
    assert!(fs::read_to_string(root.join("resume.txt"))
        .unwrap()
        .contains("best=40"));
}
