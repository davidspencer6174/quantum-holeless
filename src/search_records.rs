use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::board::{QuantumBoard, MAX_TILES};
use crate::checkpoint::Checkpoint;
use crate::io::format_board;
use crate::qstate::{Assignment, StartSlot};
use crate::search_log::println;
use crate::types::{Dir, FlatIdx, Tile};

thread_local! {
    static CAPTURE: RefCell<Option<Option<QuantumBoard>>> = const { RefCell::new(None) };
    static ROOT: RefCell<Option<RootStats>> = const { RefCell::new(None) };
}

pub(crate) struct Capture(bool);

impl Capture {
    pub(crate) fn start() -> Self {
        CAPTURE.with(|capture| {
            assert!(capture.borrow().is_none());
            *capture.borrow_mut() = Some(None);
        });
        Self(true)
    }

    pub(crate) fn finish(mut self) -> Option<QuantumBoard> {
        self.0 = false;
        CAPTURE.with(|capture| capture.borrow_mut().take().flatten())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if self.0 {
            CAPTURE.with(|capture| {
                capture.borrow_mut().take();
            });
        }
    }
}

pub(crate) fn capture_witness(board: &QuantumBoard, slots: &[StartSlot], assignment: &Assignment) {
    CAPTURE.with(|capture| {
        let mut capture = capture.borrow_mut();
        let Some(candidate) = capture.as_mut() else {
            return;
        };
        if candidate.is_some() {
            return;
        }
        let mut concrete = board.clone();
        for tile in &mut concrete.tiles {
            if matches!(tile, Tile::Quantum(_)) {
                *tile = Tile::Floor;
            }
        }
        concrete.quantum_blocks.clear();
        for &index in assignment {
            let slot = slots[index as usize];
            concrete.tiles[slot.start_pos as usize] =
                if slot.is_wall { Tile::Wall } else { Tile::Box };
            concrete.box_push_masks[slot.start_pos as usize] =
                if slot.is_wall { 0 } else { slot.push_mask };
        }
        *candidate = Some(concrete);
    });
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RouteMetrics {
    pushes: u32,
    push_edges: u32,
    box_directions: u32,
    boxes_pushed: u32,
}

impl RouteMetrics {
    fn scores(self) -> [u32; 3] {
        [self.pushes, self.push_edges, self.box_directions]
    }
    fn line(self) -> String {
        format!(
            "pushes={} push_edges={} box_directions={} boxes_pushed={}",
            self.pushes, self.push_edges, self.box_directions, self.boxes_pushed
        )
    }
}

/// Replay one already-proven shortest route, preserving concrete box identities.
pub(crate) fn route_metrics(
    board: &QuantumBoard,
    exit: FlatIdx,
    path: &[Dir],
) -> Option<RouteMetrics> {
    if !board.quantum_blocks.is_empty() || matches!(board.get_flat(exit), Tile::Box | Tile::Wall) {
        return None;
    }
    let mut player = board.find_player();
    let mut tiles = board.tiles;
    tiles[player as usize] = Tile::Floor;
    let mut masks = board.box_push_masks;
    let mut ids = [0; MAX_TILES];
    for i in 0..MAX_TILES {
        ids[i] = i;
    }
    let mut edges = [0u8; MAX_TILES];
    let mut directions = [0u8; MAX_TILES];
    let mut pushes = 0;
    for (step, &dir) in path.iter().enumerate() {
        if player == exit {
            return None;
        }
        let (x, y) = board.coords(player);
        let (x, y) = dir.apply(x, y)?;
        if !board.in_inner(x, y) {
            return None;
        }
        let next = board.flat(x, y) as usize;
        match tiles[next] {
            Tile::Floor => {}
            Tile::Box => {
                if masks[next] & dir.mask() == 0 {
                    return None;
                }
                let (x, y) = dir.apply(x, y)?;
                if !board.in_inner(x, y) {
                    return None;
                }
                let beyond = board.flat(x, y) as usize;
                if tiles[beyond] != Tile::Floor {
                    return None;
                }
                tiles[beyond] = Tile::Box;
                tiles[next] = Tile::Floor;
                masks[beyond] = masks[next];
                ids[beyond] = ids[next];
                edges[next] |= dir.mask();
                directions[ids[next]] |= dir.mask();
                pushes += 1;
            }
            _ => return None,
        }
        player = next as FlatIdx;
        if player == exit && step + 1 != path.len() {
            return None;
        }
    }
    (player == exit).then(|| RouteMetrics {
        pushes,
        push_edges: edges.iter().map(|mask| mask.count_ones()).sum(),
        box_directions: directions.iter().map(|mask| mask.count_ones()).sum(),
        boxes_pushed: directions.iter().filter(|&&mask| mask != 0).count() as u32,
    })
}

struct RootStats {
    id: usize,
    prefix: String,
    started: Instant,
    visits: u64,
    dedup_hits: u64,
    bfs_calls: u64,
    bfs_time: Duration,
    best_steps: usize,
    best_body: String,
    last_saved: Option<Instant>,
}

const SUMMARY_HEADER: &str =
    "id\tprefix\tstatus\tbest_steps\tvisits\tdedup_hits\tbfs_calls\tbfs_seconds\telapsed_seconds\n";
const METRIC_FILES: [&str; 3] = [
    "most-pushes.txt",
    "most-push-edges.txt",
    "most-box-directions.txt",
];

fn improves(score: u32, best: u32) -> bool {
    // MAX denotes no witness yet; a genuine zero-push record is still useful.
    best == u32::MAX || score > best
}

impl RootStats {
    fn row(&self, status: &str) -> String {
        format!(
            "{}\t{}\t{status}\t{}\t{}\t{}\t{}\t{:.6}\t{:.6}\n",
            self.id,
            self.prefix,
            self.best_steps,
            self.visits,
            self.dedup_hits,
            self.bfs_calls,
            self.bfs_time.as_secs_f64(),
            self.started.elapsed().as_secs_f64()
        )
    }

    fn snapshot(&self, status: &str) -> String {
        format!("{SUMMARY_HEADER}{}\n{}", self.row(status), self.best_body)
    }
}

fn root_name(id: usize) -> String {
    format!("subtrees/root-{id:08}.txt")
}

pub(crate) fn local_candidate(depth: usize) -> bool {
    ROOT.with(|root| {
        root.borrow()
            .as_ref()
            .is_some_and(|root| depth > root.best_steps)
    })
}

pub(crate) fn note_visit(deduplicated: bool) {
    ROOT.with(|root| {
        if let Some(root) = root.borrow_mut().as_mut() {
            if deduplicated {
                root.dedup_hits += 1;
            } else {
                root.visits += 1;
            }
        }
    });
}

pub(crate) struct SearchRecords {
    pub(crate) directory: PathBuf,
    writer: Mutex<SummaryWriter>,
    scores: [AtomicU32; 3],
}

struct SummaryWriter {
    contents: Vec<u8>,
    written: usize,
    rewrite: bool,
}

impl SummaryWriter {
    fn flush(&mut self, directory: &std::path::Path) {
        let path = directory.join("subtree_summary.tsv");
        let result = (|| -> io::Result<()> {
            if self.rewrite {
                File::create(&path)?.write_all(&self.contents)?;
            } else {
                OpenOptions::new()
                    .append(true)
                    .open(&path)?
                    .write_all(&self.contents[self.written..])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.written = self.contents.len();
                self.rewrite = false;
            }
            Err(error) => {
                // A failed write may have appended only part of a row.
                self.rewrite = true;
                crate::search_log::warn_output(&path, &error);
            }
        }
    }
}

impl SearchRecords {
    pub(crate) fn new(directory: PathBuf, checkpoint: Option<&Checkpoint>) -> io::Result<Self> {
        fs::create_dir_all(directory.join("subtrees"))?;
        let scores = std::array::from_fn(|i| {
            let text = if let Some(checkpoint) = checkpoint {
                checkpoint
                    .artifact(METRIC_FILES[i])
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            } else {
                fs::read_to_string(directory.join(METRIC_FILES[i])).ok()
            };
            let score = text
                .and_then(|text| text.lines().next()?.strip_prefix("score=")?.parse().ok())
                .unwrap_or(u32::MAX);
            AtomicU32::new(score)
        });
        let mut rows = Vec::new();
        let snapshots: Vec<_> = if let Some(checkpoint) = checkpoint {
            checkpoint
                .artifacts()
                .filter(|(name, _)| name.starts_with("subtrees/"))
                .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
                .collect()
        } else {
            fs::read_dir(directory.join("subtrees"))?
                .map(|file| fs::read_to_string(file?.path()))
                .collect::<io::Result<_>>()?
        };
        for text in snapshots {
            if let Some(row) = text.lines().nth(1) {
                let id: usize = row
                    .split('\t')
                    .next()
                    .and_then(|id| id.parse().ok())
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid subtree record")
                    })?;
                rows.push((id, row.to_owned()));
            }
        }
        rows.sort_unstable_by_key(|(id, _)| *id);
        let mut contents = SUMMARY_HEADER.as_bytes().to_vec();
        for (_, row) in rows {
            writeln!(contents, "{row}")?;
        }
        let mut summary = SummaryWriter {
            contents,
            written: 0,
            rewrite: true,
        };
        summary.flush(&directory);
        let records = Self {
            directory,
            writer: Mutex::new(summary),
            scores,
        };
        Ok(records)
    }

    fn put(
        &self,
        checkpoint: Option<&Mutex<Checkpoint>>,
        name: &str,
        text: &str,
    ) -> io::Result<()> {
        if let Some(checkpoint) = checkpoint {
            checkpoint
                .lock()
                .unwrap()
                .put_artifact(name, text.as_bytes())
        } else {
            let path = self.directory.join(name);
            fs::create_dir_all(path.parent().unwrap())?;
            if let Err(error) =
                File::create(&path).and_then(|mut file| file.write_all(text.as_bytes()))
            {
                crate::search_log::warn_output(&path, &error);
            }
            Ok(())
        }
    }

    pub(crate) fn begin_root(
        &self,
        checkpoint: Option<&Mutex<Checkpoint>>,
        id: usize,
        prefix: String,
    ) -> io::Result<()> {
        let saved = if let Some(checkpoint) = checkpoint {
            checkpoint
                .lock()
                .unwrap()
                .artifact(&root_name(id))
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        } else {
            fs::read_to_string(self.directory.join(root_name(id))).ok()
        };
        let (best_steps, best_body) = saved
            .as_deref()
            .and_then(|text| {
                let steps = text
                    .lines()
                    .nth(1)?
                    .split('\t')
                    .nth(3)?
                    .parse::<usize>()
                    .ok()?;
                let (_, body) = text.split_once("\n\n")?;
                Some((steps, body.to_owned()))
            })
            .unwrap_or_default();
        let root = RootStats {
            id,
            prefix,
            started: Instant::now(),
            visits: 0,
            dedup_hits: 0,
            bfs_calls: 0,
            bfs_time: Duration::ZERO,
            best_steps,
            best_body,
            last_saved: None,
        };
        ROOT.with(|current| {
            assert!(current.borrow().is_none());
            *current.borrow_mut() = Some(root);
        });
        Ok(())
    }

    pub(crate) fn finish_root(&self, checkpoint: Option<&Mutex<Checkpoint>>) -> io::Result<()> {
        let root = ROOT.with(|current| current.borrow_mut().take()).unwrap();
        let mut writer = self.writer.lock().unwrap();
        self.put(checkpoint, &root_name(root.id), &root.snapshot("finished"))?;
        writer
            .contents
            .extend_from_slice(root.row("finished").as_bytes());
        writer.flush(&self.directory);
        println!("[subtree:summary] id={} best_steps={} visits={} dedup_hits={} bfs_calls={} bfs_seconds={:.3} elapsed_seconds={:.3}",
            root.id, root.best_steps, root.visits, root.dedup_hits, root.bfs_calls,
            root.bfs_time.as_secs_f64(), root.started.elapsed().as_secs_f64());
        Ok(())
    }

    pub(crate) fn observe(
        &self,
        checkpoint: Option<&Mutex<Checkpoint>>,
        board: &QuantumBoard,
        witness: Option<QuantumBoard>,
        exit: FlatIdx,
        path: &[Dir],
        elapsed: Duration,
        publishable: bool,
    ) -> io::Result<()> {
        ROOT.with(|root| {
            if let Some(root) = root.borrow_mut().as_mut() {
                root.bfs_calls += 1;
                root.bfs_time += elapsed;
            }
        });
        if !publishable {
            return Ok(());
        }
        let metrics = witness
            .as_ref()
            .and_then(|concrete| route_metrics(concrete, exit, path));
        let local = local_candidate(path.len());
        let competitive = metrics.is_some_and(|metrics| {
            metrics
                .scores()
                .iter()
                .zip(&self.scores)
                .any(|(&score, best)| improves(score, best.load(Ordering::Relaxed)))
        });
        if !local && !competitive {
            return Ok(());
        }
        let solution: String = path
            .iter()
            .map(|dir| match dir {
                Dir::Up => 'U',
                Dir::Down => 'D',
                Dir::Left => 'L',
                Dir::Right => 'R',
            })
            .collect();
        let saved_board = witness
            .as_ref()
            .filter(|_| metrics.is_some())
            .unwrap_or(board);
        let diagram: String = format_board(saved_board, exit)
            .lines()
            .filter(|line| *line != "Letters:" && *line != "Push masks:")
            .map(|line| format!("{line}\n"))
            .collect();
        let (x, y) = board.coords(exit);
        let body = format!("steps={} solution={solution}\n{}\nwitness={}\nInput:\nexit {x},{y}\nsteps {}\n{diagram}", path.len(),
            metrics.map_or_else(|| "route metrics unavailable; clean quantum family".to_owned(), RouteMetrics::line),
            if metrics.is_some() { "concrete" } else { "quantum-family" }, path.len());
        let root_update = ROOT.with(|root| {
            let mut root = root.borrow_mut();
            let Some(root) = root.as_mut() else {
                return None;
            };
            if !local {
                return None;
            }
            root.best_steps = path.len();
            root.best_body = body.clone();
            let snapshot = if root
                .last_saved
                .is_none_or(|saved| saved.elapsed() >= Duration::from_secs(30))
            {
                root.last_saved = Some(Instant::now());
                Some(root.snapshot("running"))
            } else {
                None
            };
            Some((root.id, snapshot))
        });
        let origin = ROOT.with(|root| {
            root.borrow().as_ref().map_or_else(
                || "root=coordinator".to_owned(),
                |root| format!("root={} prefix={}", root.id, root.prefix),
            )
        });
        if let Some((id, _)) = &root_update {
            println!(
                "[subtree:record] id={id} steps={} {}",
                path.len(),
                metrics.map_or_else(|| "metrics=unavailable".to_owned(), RouteMetrics::line)
            );
        }
        if !competitive
            && root_update
                .as_ref()
                .is_none_or(|(_, snapshot)| snapshot.is_none())
        {
            return Ok(());
        }
        let _writer = self.writer.lock().unwrap();
        if let Some((id, Some(snapshot))) = root_update {
            self.put(checkpoint, &root_name(id), &snapshot)?;
        }
        if let Some(metrics) = metrics {
            for (i, score) in metrics.scores().into_iter().enumerate() {
                if !improves(score, self.scores[i].load(Ordering::Relaxed)) {
                    continue;
                }
                self.put(
                    checkpoint,
                    METRIC_FILES[i],
                    &format!("score={score}\n{origin}\n{body}"),
                )?;
                self.scores[i].store(score, Ordering::Relaxed);
                println!(
                    "[metric:record] kind={} score={score} steps={} {origin}",
                    METRIC_FILES[i].trim_end_matches(".txt"),
                    path.len()
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bfs::{forward_bfs, BfsResult};

    #[cfg(windows)]
    #[test]
    fn locked_summary_does_not_stop_roots_and_pending_rows_are_recovered() {
        use std::os::windows::fs::OpenOptionsExt;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scratch/tests-locked-summary")
            .join(format!("{}-{stamp}", std::process::id()));
        let records = SearchRecords::new(directory.clone(), None).unwrap();
        let path = directory.join("subtree_summary.tsv");
        // Match a spreadsheet reader permitting reads but denying writes/deletes.
        let viewer = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        for id in 1..=2 {
            records.begin_root(None, id, "U".to_owned()).unwrap();
            records.finish_root(None).unwrap();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), SUMMARY_HEADER);
        drop(records);
        // Startup must also work with the old summary still locked.
        let records = SearchRecords::new(directory.clone(), None).unwrap();
        drop(viewer);
        records.begin_root(None, 3, "D".to_owned()).unwrap();
        records.finish_root(None).unwrap();
        let rows = fs::read_to_string(&path).unwrap();
        let ids: Vec<_> = rows
            .lines()
            .skip(1)
            .map(|row| row.split('\t').next().unwrap())
            .collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    fn moves(text: &str) -> Vec<Dir> {
        text.chars()
            .map(|c| match c {
                'U' => Dir::Up,
                'D' => Dir::Down,
                'L' => Dir::Left,
                'R' => Dir::Right,
                _ => panic!(),
            })
            .collect()
    }

    #[test]
    fn counts_repeated_edges_and_box_directions_with_moving_masks() {
        let mut board = QuantumBoard::new(5, 5);
        for x in 1..=5 {
            for y in 1..=5 {
                board.set(x, y, Tile::Floor);
            }
        }
        board.set(2, 3, Tile::Player);
        board.set(3, 3, Tile::Box);
        board.set_box_push_mask(board.flat(3, 3), Dir::Left.mask() | Dir::Right.mask());
        let exit = board.flat(2, 4);
        let path = moves("RURRDLULLDRDL");
        assert_eq!(
            route_metrics(&board, exit, &path),
            Some(RouteMetrics {
                pushes: 3,
                push_edges: 2,
                box_directions: 2,
                boxes_pushed: 1,
            })
        );
        board.set(3, 4, Tile::Box);
        board.set_box_push_mask(board.flat(3, 4), Dir::Down.mask());
        assert_eq!(
            route_metrics(&board, exit, &path),
            Some(RouteMetrics {
                pushes: 4,
                push_edges: 3,
                box_directions: 3,
                boxes_pushed: 2,
            })
        );
        board.set_box_push_mask(board.flat(3, 3), Dir::Right.mask());
        assert!(route_metrics(&board, exit, &path).is_none());
    }

    #[test]
    fn witness_capture_selects_a_clean_assignment_and_validates_the_given_route() {
        let parsed = crate::io::parse_input("exit 3,1\nsteps 2\n4a0A\n").unwrap();
        let capture = Capture::start();
        let path = moves("RR");
        let result = forward_bfs(&parsed.board, parsed.exit, 2, &path, &[], false, 0, None);
        assert!(matches!(
            result,
            BfsResult::SomeClean {
                publishable: true,
                ..
            }
        ));
        let witness = capture.finish().unwrap();
        assert!(witness.quantum_blocks.is_empty());
        assert_eq!(witness.get(2, 1), Tile::Floor);
        assert_eq!(witness.get(4, 1), Tile::Box);
        assert_eq!(
            route_metrics(&witness, parsed.exit, &path).unwrap().pushes,
            0
        );
        let covered = witness.flat(4, 1);
        assert!(route_metrics(&witness, covered, &path).is_none());
        assert!(route_metrics(&witness, parsed.exit, &moves("RRR")).is_none());
        assert!(route_metrics(&witness, parsed.exit, &moves("L")).is_none());
        assert!(Capture::start().finish().is_none());
        let covered = crate::io::parse_input("exit 3,1\nsteps 2\n4020\n").unwrap();
        let capture = Capture::start();
        assert!(matches!(
            forward_bfs(&covered.board, covered.exit, 2, &path, &[], false, 0, None),
            BfsResult::SomeClean {
                publishable: false,
                ..
            }
        ));
        assert!(capture.finish().is_none());
    }
}
