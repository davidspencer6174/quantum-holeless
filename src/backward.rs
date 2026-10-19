// backward.rs — generate all boards reachable via one "undo" move.

use rustc_hash::FxHashMap;
use rustc_hash::FxHasher;
use smallvec::SmallVec;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::PathBuf;

use crate::bfs::{forward_bfs, BfsResult};
use crate::board::QuantumBoard;
use crate::io::{format_board, format_board_header};
use crate::progress::{print_progress, ProgressReporter, SearchProgress, REPORT_INTERVAL};
use crate::shortcut_paths::ShortcutPaths;
use crate::types::{fis_single, Dir, FlatIdx, FlatIdxSet, QBlockId, QuantumBlock, Tile};
use std::fs::OpenOptions;
use std::io::Write;
use crate::search_log::{self, print, println, SearchLog};
use crate::work_queue::WorkQueue;

// ── Board fingerprint ─────────────────────────────────────────────────────────

fn board_fingerprint(board: &QuantumBoard) -> u64 {
    let mut h = FxHasher::default();
    board.width.hash(&mut h);
    board.height.hash(&mut h);

    let total = (board.width * board.height) as usize;

    // 1. Map old IDs to canonical IDs based on row-major appearance order.
    let mut old_to_canonical = [u8::MAX; 256];
    let mut appearance_order = SmallVec::<[QBlockId; 16]>::new();
    let mut next_canonical = 0u8;

    for &tile in &board.tiles[..total] {
        if let Tile::Quantum(old_id) = tile {
            if old_to_canonical[old_id as usize] == u8::MAX {
                appearance_order.push(old_id);
                old_to_canonical[old_id as usize] = next_canonical;
                next_canonical += 1;
            }
        }
    }

    // 2. Hash tiles using canonical IDs.
    for &tile in &board.tiles[..total] {
        match tile {
            Tile::Quantum(id) => Tile::Quantum(old_to_canonical[id as usize]).hash(&mut h),
            other => other.hash(&mut h),
        }
    }

    // 3. Hash QuantumBlock data in the stable appearance order.
    for &old_id in &appearance_order {
        let qb = &board.quantum_blocks[old_id as usize];
        let mut cells: SmallVec<[(FlatIdx, &FlatIdxSet); 8]> = qb
            .cells
            .iter()
            .map(|(&pos, starts)| (pos, starts))
            .collect();
        cells.sort_unstable_by_key(|&(pos, _)| pos);
        for (pos, starts) in cells {
            pos.hash(&mut h);
            starts.as_slice().hash(&mut h);
        }
        qb.wall_pos.hash(&mut h);
    }

    h.finish()
}

/// Returns true if the tile at `pos` is surrounded by 3 walls or board boundaries.
fn is_dead_end(board: &QuantumBoard, pos: FlatIdx) -> bool {
    let (x, y) = board.coords(pos);
    let mut solids = 0;
    for dir in Dir::all() {
        match dir.apply(x, y) {
            Some((nx, ny)) if board.in_inner(nx, ny) => {
                if board.get(nx, ny) == Tile::Wall {
                    solids += 1;
                }
            }
            _ => solids += 1, // Boundary
        }
    }
    solids >= 3
}

fn block_domain_analysis(
    board: &QuantumBoard,
    target: FlatIdx,
    start_pos: FlatIdx,
    start_mask: u8,
    exit: FlatIdx,
) -> bool {
    let total = (board.width * board.height) as usize;
    let mut visited = vec![false; total];
    let mut queue = VecDeque::new();

    visited[start_pos as usize] = true;
    queue.push_back(start_pos);

    while let Some(cur_pos) = queue.pop_front() {
        if cur_pos == target {
            return true;
        }

        if start_mask == 0 {
            return false;
        }

        let (x, y) = board.coords(cur_pos);
        for dir in Dir::all() {
            if start_mask & dir.mask() == 0 {
                continue;
            }
            let src_coords = match dir.opposite().apply(x, y) {
                Some(coords) if board.in_inner(coords.0, coords.1) => coords,
                _ => continue,
            };
            let dst_coords = match dir.apply(x, y) {
                Some(coords) if board.in_inner(coords.0, coords.1) => coords,
                _ => continue,
            };

            let src = board.flat(src_coords.0, src_coords.1);
            let dst = board.flat(dst_coords.0, dst_coords.1);

            if src == exit {
                continue;
            }
            if board.get_flat(src) == Tile::Wall || board.get_flat(dst) == Tile::Wall {
                continue;
            }

            let dst_idx = dst as usize;
            if !visited[dst_idx] {
                visited[dst_idx] = true;
                queue.push_back(dst);
            }
        }
    }

    false
}

fn can_block_reach_dead_end(board: &QuantumBoard, p_idx: FlatIdx, exit: FlatIdx) -> bool {
    for idx in 0..(board.width * board.height) {
        let idx = idx as FlatIdx;
        if board.get_flat(idx) == Tile::Box {
            let push_mask = board.get_box_push_mask(idx);
            if push_mask != 0 && block_domain_analysis(board, p_idx, idx, push_mask, exit) {
                return true;
            }
        }
    }

    for qb in &board.quantum_blocks {
        for pos in qb.current_positions() {
            if !qb.can_be_box_at(pos) {
                continue;
            }
            let push_mask = qb.box_push_mask_at(pos);
            if push_mask == 0 {
                continue;
            }
            if block_domain_analysis(board, p_idx, pos, push_mask, exit) {
                return true;
            }
        }
    }

    false
}

// ── UndoKind ──────────────────────────────────────────────────────────────────
//
// PureMove: no block was pulled. Tracks the tile type that the player stepped onto.
//
// Pull { qb_id, pulled_pos }: a block landed at pulled_pos (the player's old
//   position). If the block started at pulled_pos the push/pull cancel — a
//   shortcut. We remove pulled_pos from the QB before BFS.

#[derive(Debug, Clone, Copy)]
pub enum UndoKind {
    PureMove { original_dest_tile: Tile },
    Pull {
        qb_id: QBlockId,
        pulled_pos: FlatIdx,
        original_dest_tile: Tile,
    },
}

// ── Undo moves ────────────────────────────────────────────────────────────────

fn undo_move(
    board: &QuantumBoard,
    dir: Dir,
    required_walls: u128,
) -> Vec<(QuantumBoard, UndoKind)> {
    let player = board.find_player();
    let (px, py) = board.coords(player);

    let from = match dir.apply(px, py) {
        Some((fx, fy)) if board.in_inner(fx, fy) => board.flat(fx, fy),
        _ => return vec![],
    };
    let from_tile = board.get_flat(from);

    if matches!(from_tile, Tile::Box | Tile::Player) {
        return vec![];
    }

    let push_src = dir
        .opposite()
        .apply(px, py)
        .filter(|&(x, y)| board.in_inner(x, y))
        .map(|(x, y)| board.flat(x, y));
    let push_src_tile = push_src.map(|f| board.get_flat(f));

    let mut base = board.clone();
    match from_tile {
        Tile::Floor => {}
        Tile::Wall => {
            if (required_walls & (1u128 << from)) != 0 {
                return vec![];
            }
            base.set_flat(from, Tile::Floor);
        }
        Tile::Quantum(id) => {
            let became_empty = base.qb_remove_current(id, from);
            if became_empty {
                return vec![];
            }
        }
        Tile::Box | Tile::Player => return vec![],
    }

    base.set_flat(player, Tile::Floor);
    base.set_flat(from, Tile::Player);
    let push_direction_mask = dir.opposite().mask();

    let kind = match (push_src, push_src_tile) {
        (None, _) | (_, Some(Tile::Floor)) => UndoKind::PureMove { original_dest_tile: from_tile },

        (Some(src), Some(Tile::Wall)) => {
            let new_id = base.quantum_blocks.len() as QBlockId;
            let mut cells: FxHashMap<FlatIdx, FlatIdxSet> = FxHashMap::default();
            cells.insert(src, fis_single(src));
            cells.insert(player, fis_single(player));
            let mut push_masks: FxHashMap<FlatIdx, crate::types::PushMask> = FxHashMap::default();
            push_masks.insert(src, crate::types::PUSH_NONE);
            push_masks.insert(player, push_direction_mask);
            base.set_flat(src, Tile::Quantum(new_id));
            base.set_flat(player, Tile::Quantum(new_id));
            base.quantum_blocks
                .push(QuantumBlock::new_with_push_masks(cells, Some(src), push_masks));
            UndoKind::Pull {
                qb_id: new_id,
                pulled_pos: player,
                original_dest_tile: from_tile,
            }
        }

        (Some(src), Some(Tile::Box)) => {
            let new_id = base.quantum_blocks.len() as QBlockId;
            let mut cells: FxHashMap<FlatIdx, FlatIdxSet> = FxHashMap::default();
            cells.insert(src, fis_single(src));
            cells.insert(player, fis_single(player));
            let src_mask = base.get_box_push_mask(src);
            let player_mask = src_mask | push_direction_mask;
            let mut push_masks: FxHashMap<FlatIdx, crate::types::PushMask> = FxHashMap::default();
            push_masks.insert(src, src_mask);
            push_masks.insert(player, player_mask);
            base.set_flat(src, Tile::Quantum(new_id));
            base.set_flat(player, Tile::Quantum(new_id));
            base.quantum_blocks.push(QuantumBlock::new_with_push_masks(cells, None, push_masks));
            UndoKind::Pull {
                qb_id: new_id,
                pulled_pos: player,
                original_dest_tile: from_tile,
            }
        }

        (Some(src), Some(Tile::Quantum(push_id))) => {
            let old_mask = base.quantum_blocks[push_id as usize].box_push_mask_at(push_src.unwrap());
            let player_mask = old_mask | push_direction_mask;
            base.set_flat(player, Tile::Quantum(push_id));
            base.quantum_blocks[push_id as usize]
                .cells
                .insert(player, fis_single(player));
            base.quantum_blocks[push_id as usize]
                .push_masks
                .insert(player, player_mask);
            UndoKind::Pull {
                qb_id: push_id,
                pulled_pos: player,
                original_dest_tile: from_tile,
            }
        }

        (_, Some(Tile::Player)) | (_, None) => UndoKind::PureMove { original_dest_tile: from_tile },
    };

    vec![(base, kind)]
}

// ── Apply ruled_out to a board ────────────────────────────────────────────────

fn apply_ruled_out(board: &mut QuantumBoard, ruled_out: &[(usize, FlatIdx)]) {
    for &(qb_id, start_pos) in ruled_out {
        if qb_id >= board.quantum_blocks.len() {
            continue;
        }
        let cur_positions: Vec<FlatIdx> =
            board.quantum_blocks[qb_id].cells.keys().cloned().collect();
        for pos in cur_positions {
            if let Some(starts) = board.quantum_blocks[qb_id].cells.get_mut(&pos) {
                crate::types::fis_remove(starts, start_pos);
                if starts.is_empty() {
                    board.quantum_blocks[qb_id].cells.remove(&pos);
                    if board.tiles[pos as usize] == Tile::Quantum(qb_id as QBlockId) {
                        board.tiles[pos as usize] = Tile::Floor;
                    }
                }
            }
        }
        if board.quantum_blocks[qb_id].wall_pos == Some(start_pos) {
            board.quantum_blocks[qb_id].wall_pos = None;
        }
    }
}

// ── BFS timing histogram ──────────────────────────────────────────────────────

struct BfsTimingStats {
    lt1ms: u64,
    lt10ms: u64,
    lt100ms: u64,
    lt1s: u64,
    lt10s: u64,
    ge10s: u64,
    total: u64,
    allshortcut: u128,
    someclean: u128,
    indeterminate: u128,
}

impl BfsTimingStats {
    fn new() -> Self {
        Self {
            lt1ms: 0,
            lt10ms: 0,
            lt100ms: 0,
            lt1s: 0,
            lt10s: 0,
            ge10s: 0,
            total: 0,
            allshortcut: 0,
            someclean: 0,
            indeterminate: 0,
        }
    }

    fn record(&mut self, elapsed_us: u128, bfs_result: &BfsResult) {
        self.total += 1;
        match elapsed_us {
            t if t < 1_000 => self.lt1ms += 1,
            t if t < 10_000 => self.lt10ms += 1,
            t if t < 100_000 => self.lt100ms += 1,
            t if t < 1_000_000 => self.lt1s += 1,
            t if t < 10_000_000 => self.lt10s += 1,
            _ => self.ge10s += 1,
        }
        match bfs_result {
            BfsResult::Unreachable => {}
            BfsResult::AllShortcut => self.allshortcut += elapsed_us,
            BfsResult::SomeClean { .. } => self.someclean += elapsed_us,
            BfsResult::Indeterminate { .. } => self.indeterminate += elapsed_us,
        }
    }

    fn print(&self) {
        println!(
            "=== BFS timing distribution ({} total calls) ===",
            self.total
        );
        let pct = |n: u64| {
            if self.total > 0 {
                100.0 * n as f64 / self.total as f64
            } else {
                0.0
            }
        };
        println!("  < 1ms:      {:>8}  ({:.1}%)", self.lt1ms, pct(self.lt1ms));
        println!("  1–10ms:     {:>8}  ({:.1}%)", self.lt10ms, pct(self.lt10ms));
        println!("  10–100ms:   {:>8}  ({:.1}%)", self.lt100ms, pct(self.lt100ms));
        println!("  100ms–1s:   {:>8}  ({:.1}%)", self.lt1s, pct(self.lt1s));
        println!("  1s–10s:     {:>8}  ({:.1}%)", self.lt10s, pct(self.lt10s));
        println!("  ≥ 10s:      {:>8}  ({:.1}%)", self.ge10s, pct(self.ge10s));
        println!(
            "Total time on AllShortcut results: {} ms",
            self.allshortcut as f64 / 1000.0
        );
        println!(
            "Total time on SomeClean results: {} ms",
            self.someclean as f64 / 1000.0
        );
        println!(
            "Total time on Indeterminate results: {} ms",
            self.indeterminate as f64 / 1000.0
        );
    }
}

// ── Heuristic Tracker Stats ───────────────────────────────────────────────────

struct HeuristicStats {
    h_triggered: u64,
    h_some_clean_empty_ruled_out: u64,
    h_some_clean_with_ruled_out: u64,
    h_all_shortcut: u64,
    h_indeterminate: u64,
}

impl HeuristicStats {
    fn new() -> Self {
        Self {
            h_triggered: 0,
            h_some_clean_empty_ruled_out: 0,
            h_some_clean_with_ruled_out: 0,
            h_all_shortcut: 0,
            h_indeterminate: 0,
        }
    }

    fn record_match(&mut self, result: &BfsResult) {
        self.h_triggered += 1;
        match result {
            BfsResult::Unreachable => {}
            BfsResult::AllShortcut => self.h_all_shortcut += 1,
            BfsResult::Indeterminate { .. } => self.h_indeterminate += 1,
            BfsResult::SomeClean { ruled_out, .. } => {
                if ruled_out.is_empty() {
                    self.h_some_clean_empty_ruled_out += 1;
                } else {
                    self.h_some_clean_with_ruled_out += 1;
                }
            }
        }
    }

    fn print(&self) {
        println!("=== Heuristic Analysis ({} Triggers) ===", self.h_triggered);
        if self.h_triggered == 0 {
            println!("  No instances flagged by heuristic.");
            return;
        }
        let pct = |n: u64| 100.0 * n as f64 / self.h_triggered as f64;
        println!(
            "  SomeClean (Empty ruled_out):  {:>8}  ({:.1}%)",
            self.h_some_clean_empty_ruled_out,
            pct(self.h_some_clean_empty_ruled_out)
        );
        println!(
            "  SomeClean (With ruled_out):   {:>8}  ({:.1}%)",
            self.h_some_clean_with_ruled_out,
            pct(self.h_some_clean_with_ruled_out)
        );
        println!(
            "  AllShortcut:                  {:>8}  ({:.1}%)",
            self.h_all_shortcut,
            pct(self.h_all_shortcut)
        );
        println!(
            "  Indeterminate:                {:>8}  ({:.1}%)",
            self.h_indeterminate,
            pct(self.h_indeterminate)
        );
    }
}

struct SharedState {
    seen: FxHashMap<u64, usize>,
    longest_found: usize,
    slowest_millis: u64,
    timing: BfsTimingStats,
    heuristic_stats: HeuristicStats,
    progress: Vec<SearchProgress>,
    subtrees_queued: usize,
    subtrees_completed: usize,
    frozen_exit_prunes: usize,
    symmetry_prunes: usize,
}

impl SharedState {
    fn new(width: u16, height: u16, exit: (u16, u16), progress_count: usize) -> Self {
        Self {
            seen: FxHashMap::default(),
            longest_found: 0,
            slowest_millis: 0,
            timing: BfsTimingStats::new(),
            heuristic_stats: HeuristicStats::new(),
            progress: (0..progress_count).map(|_| SearchProgress::new(width, height, exit)).collect(),
            subtrees_queued: 0,
            subtrees_completed: 0,
            frozen_exit_prunes: 0,
            symmetry_prunes: 0,
        }
    }

    fn progress_message(&self, reason: &str) -> String {
        self.progress_message_for(search_log::worker_id(), reason)
    }

    fn progress_message_for(&self, worker: usize, reason: &str) -> String {
        let message = self.progress[worker].message(reason, self.seen.len(), self.longest_found, self.slowest_millis);
        if self.progress.len() > 1 { format!("[worker:{worker:02}] {message}") } else { message }
    }
}

#[derive(Clone, Debug)]
pub struct BackwardSearchOptions {
    pub threads: usize,
    pub split_depth: usize,
    pub log_dir: PathBuf,
    pub record_dir: PathBuf,
    pub checkpoint_dir: Option<PathBuf>,
    pub resume: bool,
    pub symmetry: bool,
    pub extra_records: bool,
}

impl Default for BackwardSearchOptions {
    fn default() -> Self {
        Self { threads: 1, split_depth: 10, log_dir: PathBuf::from("logs"), record_dir: PathBuf::from("."),
            checkpoint_dir: None, resume: false, symmetry: true, extra_records: true }
    }
}

#[derive(Debug)]
pub struct BackwardSearchSummary {
    pub longest_found: usize,
    pub unique_boards: usize,
    pub subtrees_completed: usize,
    pub log_directory: Option<PathBuf>,
    pub stopped: bool,
    pub checkpoint_skips: usize,
}

struct BackwardJob {
    id: usize,
    board: QuantumBoard,
    depth: usize,
    exit: FlatIdx,
    prev_undo_dir: Option<Dir>,
    path: Vec<Dir>,
    shortcuts: ShortcutPaths,
    required_walls: u128,
}

struct SearchRuntime {
    state: Mutex<SharedState>,
    queue: Option<WorkQueue<BackwardJob>>,
    split_depth: usize,
    record_path: PathBuf,
    checkpoint: Option<Mutex<crate::checkpoint::Checkpoint>>,
    // Replaying the shallow producer must not depend on worker scheduling.
    frontier_seen: Option<Mutex<FxHashMap<u64, usize>>>,
    symmetries: Vec<[Dir; 4]>,
    stopping: AtomicBool,
    checkpoint_skips: std::sync::atomic::AtomicUsize,
    records: Option<crate::search_records::SearchRecords>,
}

// ── Heuristic implementation ─────────────────────────────────────────────────


/// Determines if a direction is completely blocked for a forward player step.
/// This matches your criteria: outside bounds, a wall, or a block that cannot be pushed.
fn is_direction_hard_blocked(board: &QuantumBoard, player_idx: FlatIdx, dir: Dir) -> bool {
    let (px, py) = board.coords(player_idx);
    
    // 1. Board Edge / Outer Boundary Check
    let (nx, ny) = match dir.apply(px, py) {
        Some((x, y)) if board.in_inner(x, y) => (x, y),
        _ => return true, // Out of bounds or edge
    };

    let next_idx = board.flat(nx, ny);
    let next_tile = board.get_flat(next_idx);

    match next_tile {
        // 2. Structural Wall
        Tile::Wall => true,
        
        // 3. Concrete Block check
        Tile::Box => {
            let push_mask = board.get_box_push_mask(next_idx);
            // The push mask must allow moving in `dir` to be passable.
            // If the bit for `dir` is 0, the block is unpushable from here.
            (push_mask & dir.mask()) == 0
        }
        
        // Quantum blocks or floor are considered open or require full solver verification
        _ => false,
    }
}

fn heuristically_clean(board: &QuantumBoard, kind: UndoKind) -> bool {
    // Strengthened heuristic: Only predict clean if it's a pure move 
    // AND the destination space was already completely empty (Tile::Floor).
    let player_idx = board.find_player();
    let mut blocked_count = 0;

    for dir in Dir::all() {
        if is_direction_hard_blocked(board, player_idx, dir) {
            blocked_count += 1;
        }
    }
    return false;

    //unfortunately the below ones seem harmful. more testing needed
    /*if let UndoKind::PureMove { original_dest_tile } = kind {
        //if original_dest_tile == Tile::Floor {
        //    println!("puremove floor")
        //}
        return matches!(original_dest_tile, Tile::Floor);
    }
    if let UndoKind::Pull { qb_id, pulled_pos, original_dest_tile } = kind {
        if qb_id >= board.quantum_blocks.len() as u8 || &board.tiles[pulled_pos as usize] != &Tile::Quantum(qb_id as QBlockId) {
            return false;
        }
        //count the QB's locations
        //println!("qb id: {}", qb_id);
        //println!("quantum blocks len: {}", board.quantum_blocks.len());
        let qb = &board.quantum_blocks[qb_id as usize];
        let mut pos_count = 0;
        for pos in qb.current_positions() {
            pos_count += 1;
        }
        //if original_dest_tile == Tile::Floor && pos_count >= 3{
        //    println!("final condition");
        //}

        return original_dest_tile == Tile::Floor && pos_count >= 3;
    }
    false*/
}

// ── Backward DFS ──────────────────────────────────────────────────────────────

fn publish_record(shared: &SearchRuntime, board: &QuantumBoard, exit: FlatIdx, depth: usize) {
    // Keep the comparison, file append, and best-record update in one critical
    // section so a slower writer cannot append an older record after a new one.
    let mut state = shared.state.lock().unwrap();
    if depth <= state.longest_found { return; }
    let header = format_board_header(board, exit, &format!("Longest path so far with depth = {}", depth));
    let text = format!("{}{}\n", header, format_board(board, exit));
    if let Some(checkpoint) = &shared.checkpoint {
        checkpoint.lock().unwrap().record(depth, &text).expect("failed to checkpoint record");
    } else {
        let mut file = OpenOptions::new().create(true).append(true).open(&shared.record_path)
            .expect("failed to open record file");
        file.write_all(text.as_bytes()).expect("failed to write record");
    }
    state.longest_found = depth;
    let message = format!(">>> New record found at depth {}! Writing to {}", depth, shared.record_path.display());
    println!("{message}");
    if shared.queue.is_some() { std::println!("[worker:{:02}] {message}", search_log::worker_id()); }
}

fn solve_for_backward(
    shared: &SearchRuntime,
    board: &QuantumBoard,
    exit: FlatIdx,
    target_steps: u32,
    path: &[Dir],
    _paths: &mut ShortcutPaths,
    state_limit: Option<u32>,
) -> BfsResult {
    let capture = shared.records.as_ref().map(|_| crate::search_records::Capture::start());
    let started = std::time::Instant::now();
    #[cfg(feature = "shortcut-reuse")]
    let result = crate::bfs::forward_bfs_reusing(board, exit, target_steps, path, _paths, 0, state_limit);
    #[cfg(not(feature = "shortcut-reuse"))]
    let result = crate::bfs::forward_bfs_complete(board, exit, target_steps, path, 0, state_limit);
    let elapsed = started.elapsed();
    if let Some(records) = &shared.records {
        records.observe(shared.checkpoint.as_ref(), board, capture.and_then(crate::search_records::Capture::finish),
            exit, path, elapsed, matches!(&result, BfsResult::SomeClean { publishable: true, .. }))
            .expect("failed to write search records");
    }
    result
}

fn backward_dfs_recursive(
    board: &QuantumBoard,
    depth: usize,
    exit: FlatIdx,
    shared: &Arc<SearchRuntime>,
    prev_undo_dir: Option<Dir>,
    path: Vec<Dir>,
    mut shortcuts: ShortcutPaths,
    required_walls: u128,
    verified_clean: Option<bool>,
) {
    let step_limit_to_print_clean = 2;
    if search_log::worker_id() == 0 && shared.stopping.load(Ordering::Acquire) { return; }
    if !crate::symmetry::canonical_path(&path, &shared.symmetries) {
        shared.state.lock().unwrap().symmetry_prunes += 1;
        return;
    }
    if crate::backward_deadlock::exit_is_pull_frozen(board, exit) {
        shared.state.lock().unwrap().frozen_exit_prunes += 1;
        return;
    }

    if let Some(queue) = &shared.queue {
        if queue.is_cancelled() { return; }
        if search_log::worker_id() == 0 && depth >= shared.split_depth {
            let id = {
                let mut state = shared.state.lock().unwrap();
                state.subtrees_queued += 1;
                state.progress[0].update(&path, "queue-wait");
                state.subtrees_queued
            };
            if let Some(checkpoint) = &shared.checkpoint {
                let key = crate::checkpoint::subtree_key(board, &path, required_walls);
                if checkpoint.lock().unwrap().contains(&key) {
                    shared.checkpoint_skips.fetch_add(1, Ordering::Relaxed);
                    println!("[subtree:saved] id={id} depth={depth} moves={}", backward_moves(&path));
                    return;
                }
            }
            // The worker owns the root's visited and record checks too.
            queue.push(BackwardJob {
                id, board: board.clone(), depth, exit, prev_undo_dir, path, shortcuts, required_walls,
            });
            return;
        }
    }

    let fp = board_fingerprint(board);
    let coordinator = search_log::worker_id() == 0 && shared.frontier_seen.is_some();
    let mut record_candidate = false;
    let shallow_progress;
    {
        let mut shared_guard = shared.state.lock().unwrap();
        if coordinator {
            let mut seen = shared.frontier_seen.as_ref().unwrap().lock().unwrap();
            match seen.get(&fp).copied() {
                Some(previous) if previous >= depth => return,
                _ => { seen.insert(fp, depth); }
            }
        } else {
            match shared_guard.seen.get(&fp).copied() {
                Some(prev_depth) if prev_depth >= depth => { crate::search_records::note_visit(true); return; },
                _ => { shared_guard.seen.insert(fp, depth); }
            }
        }
        crate::search_records::note_visit(false);

        let seen_len = shared_guard.seen.len();
        if seen_len % 1000 == 0 {
            println!(
                "Visited {} unique boards so far, current depth {}, longest found {}, slowest BFS {} ms",
                seen_len,
                depth,
                shared_guard.longest_found,
                shared_guard.slowest_millis
            );
        }
        if seen_len % 1000 == 0 {
            let mut out_path = String::new();
            for p in path.iter().rev() {
                match p {
                    Dir::Up => out_path.push('U'),
                    Dir::Down => out_path.push('D'),
                    Dir::Left => out_path.push('L'),
                    Dir::Right => out_path.push('R'),
                }
            }
            println!("Path reversed: {}, seen length: {}", out_path, seen_len);
        }

        if depth > shared_guard.longest_found || (coordinator && depth > 0)
            || (board.get_flat(exit) != Tile::Box && crate::search_records::local_candidate(depth)) {
            record_candidate = true;
        }
        shared_guard.progress[search_log::worker_id()].update(&path, if record_candidate { "record-solve" } else { "backward" });
        shallow_progress = (depth < 8).then(|| shared_guard.progress_message("shallow"));
    }
    if let Some(message) = shallow_progress { print_progress(message); }

    if record_candidate {
        let start_time = std::time::Instant::now();
        let bfs_result = match verified_clean {
            Some(publishable) => BfsResult::SomeClean {
                ruled_out: Vec::new(), shortcuts: Vec::new(), publishable,
            },
            None => solve_for_backward(shared, board, exit, depth as u32, &path, &mut shortcuts, None),
        };
        let elapsed = start_time.elapsed();
        let elapsed_us = elapsed.as_micros();

        if verified_clean.is_none() {
            let mut shared_guard = shared.state.lock().unwrap();
            shared_guard.timing.record(elapsed_us, &bfs_result);
        }

        match bfs_result {
            BfsResult::Unreachable => {
                println!("Found unreachable board at depth {}:", depth);
                print!("{}", format_board_header(board, exit, "Unreachable board"));
                print!("{}", format_board(board, exit));
                panic!("Unreachable record-candidate board found! Please investigate.");
            }
            BfsResult::AllShortcut => {
                return;
            }
            BfsResult::SomeClean { publishable, .. } => {
                if publishable { publish_record(shared, board, exit, depth); }
            }
            BfsResult::Indeterminate { .. } => unreachable!("full forward_bfs returned Indeterminate"),
        }
    }

    let children: Vec<(Dir, QuantumBoard, UndoKind)> = Dir::all()
        .iter()
        .copied()
        .flat_map(|dir| {
            undo_move(board, dir, required_walls)
                .into_iter()
                .map(move |(bboard, kind)| (dir, bboard, kind))
        })
        .collect();

    let mut local_required_walls = required_walls;
    for (dir, bboard, kind) in children {
        if coordinator && shared.stopping.load(Ordering::Acquire) { return; }
        if shared.queue.as_ref().is_some_and(|queue| queue.is_cancelled()) { return; }
        process_child(
            depth, board, exit, shared, prev_undo_dir, path.clone(), shortcuts.clone(),
            &mut local_required_walls, dir, bboard, kind, step_limit_to_print_clean, record_candidate,
        );
    }
}

fn process_child(
    depth: usize,
    board: &QuantumBoard,
    exit: FlatIdx,
    shared: &Arc<SearchRuntime>,
    prev_undo_dir: Option<Dir>,
    path: Vec<Dir>,
    mut shortcuts: ShortcutPaths,
    required_walls: &mut u128,
    dir: Dir,
    mut bboard: QuantumBoard,
    kind: UndoKind,
    step_limit_to_print_clean: u32,
    record_candidate: bool,
) {
    if !shared.symmetries.is_empty() {
        let mut next_path = vec![dir.opposite()];
        next_path.extend_from_slice(&path);
        if !crate::symmetry::canonical_path(&next_path, &shared.symmetries) {
            shared.state.lock().unwrap().symmetry_prunes += 1;
            return;
        }
    }
    let child_required_walls = *required_walls;
    shortcuts.prepend(dir.opposite());
    
    if let UndoKind::PureMove { .. } = kind {
        let is_shortcut = depth > 0 && prev_undo_dir == Some(dir.opposite());
        if is_shortcut {
            return;
        }

        // --- TOPOLOGY-AWARE PROVABLE SHORTCUT CHECK ---
        if let Some(interaction_idx) = find_first_interaction_source(board, &path, exit) {
            let p_pos = board.find_player();
            let q_pos = bboard.find_player(); // New position after the backward move

            // Run flood fill over open floor tiles on the parent board starting from the interaction tile
            let floor_dists = compute_floor_distances(board, interaction_idx);


            if let (Some(dist_p), Some(dist_q)) = (floor_dists[p_pos as usize], floor_dists[q_pos as usize]) {
                // If moving backward brings us strictly closer to the interaction point via open floor space,
                // a shorter path to that interaction point exists.
                if dist_q < dist_p {
                    // Update required walls tracking for correctness before pruning
                    let (old_px, old_py) = board.coords(p_pos);
                    if let Some((fx, fy)) = dir.apply(old_px, old_py) {
                        if board.in_inner(fx, fy) {
                            let wall_pos = board.flat(fx, fy);
                            if board.get_flat(wall_pos) == Tile::Wall && wall_pos < 128 {
                                *required_walls |= 1u128 << wall_pos;
                            }
                        }
                    }
                    return; // Short-circuit: Discard this state entirely
                }
            }
        }
    }

    if let UndoKind::Pull { qb_id, pulled_pos , original_dest_tile} = kind {
        if prev_undo_dir == Some(dir.opposite()) {
            if let Some(qb) = bboard.quantum_blocks.get(qb_id as usize) {
                if qb.starts_for(pulled_pos).is_some() {
                    let box_starts = qb.box_starts_at(pulled_pos);
                    bboard.resolve_qb_to_box(qb_id, pulled_pos, box_starts);
                }
            }
        }
    }
    bboard.collapse_single_tile_qbs();
    bboard.compact_blocks();

    if crate::backward_deadlock::exit_is_pull_frozen(&bboard, exit) {
        shared.state.lock().unwrap().frozen_exit_prunes += 1;
        return;
    }

    let mut next_path = vec![dir.opposite()];
    next_path.extend(path.iter().copied());

    let target_steps = (depth + 1) as u32;

    let p_idx = bboard.find_player();
    if board.get_flat(p_idx) == Tile::Wall && is_dead_end(&bboard, p_idx) {
        if !can_block_reach_dead_end(&bboard, p_idx, exit) {
            if target_steps <= step_limit_to_print_clean
                && matches!(bboard.get_flat(exit), Tile::Floor | Tile::Player) {
                println!("Found some-clean board at depth {} via back-walk:", depth + 1);
                print!("{}", format_board_header(&bboard, exit, "Some-clean board"));
                print!("{}", format_board(&bboard, exit));
            }

            backward_dfs_recursive(
                &bboard,
                depth + 1,
                exit,
                shared,
                Some(dir),
                next_path,
                shortcuts.clone(),
                *required_walls,
                None,
            );
            return;
        }
    }

    let quick_check_only = false;
    //let quick_check_only = heuristically_clean(&bboard, kind) && !record_candidate;
    if heuristically_clean(&bboard, kind) {
        backward_dfs_recursive(
            &bboard,
            depth + 1,
            exit,
            shared,
            Some(dir),
            next_path,
            shortcuts,
            child_required_walls,
            None,
        );
        return;
    }



    let exit_tile = bboard.get_flat(exit);
    let trivially_covered = matches!(exit_tile, Tile::Box);
    if trivially_covered {
        shared.state.lock().unwrap().progress[search_log::worker_id()].update(&next_path, "exit-check");
        let mut all_pulls_fail = true;
        for pull_dir in Dir::all() {
            let (px, py) = bboard.coords(exit);
            let from = match pull_dir.apply(px, py) {
                Some((fx, fy)) if bboard.in_inner(fx, fy) => bboard.flat(fx, fy),
                _ => continue,
            };
            let from_tile = bboard.get_flat(from);
            let (from_x, from_y) = bboard.coords(from);
            let from_2 = match pull_dir.apply(from_x, from_y) {
                Some((from_x, from_y)) if bboard.in_inner(from_x, from_y) => bboard.flat(from_x, from_y),
                _ => continue,
            };
            let from_2_tile = bboard.get_flat(from_2);
            let mut wall_to_clear_tile = None;
            if from_tile == Tile::Wall && from_2_tile != Tile::Wall {
                wall_to_clear_tile = Some(from);
            }
            if from_2_tile == Tile::Wall && from_tile != Tile::Wall {
                wall_to_clear_tile = Some(from_2);
            }
            let mut test_board = bboard.clone();
            if let Some(wall_to_clear) = wall_to_clear_tile {
                test_board.set_flat(wall_to_clear, Tile::Floor);
            }
            let exit_push_mask = bboard.get_box_push_mask(exit);
            if exit_push_mask & pull_dir.opposite().mask() != 0 && wall_to_clear_tile.is_none() {
                all_pulls_fail = false;
                break;
            }
            bboard.set_box_push_mask(exit, exit_push_mask | pull_dir.opposite().mask());
            test_board.collapse_single_tile_qbs();
            test_board.compact_blocks();
            let test_path = vec![pull_dir.opposite()];
            let test_result = forward_bfs(
                &test_board,
                exit,
                target_steps,
                &test_path,
                &[],
                quick_check_only,
                0,
                Some(350_000),
            );
            if !matches!(test_result, BfsResult::AllShortcut) {
                all_pulls_fail = false;
                break;
            }
        }
        if all_pulls_fail {
            return;
        }
    }

    
    
    shared.state.lock().unwrap().progress[search_log::worker_id()].update(&next_path, "forward-solve");
    let start_time = std::time::Instant::now();
    let bfs_result = solve_for_backward(
        shared,
        &bboard,
        exit,
        target_steps,
        &next_path,
        &mut shortcuts,
        Some(10_000),
    );
    let elapsed = start_time.elapsed();
    let elapsed_us = elapsed.as_micros();
    let elapsed_ms = elapsed.as_millis() as u64;

    {
        let mut shared_guard = shared.state.lock().unwrap();
        shared_guard.timing.record(elapsed_us, &bfs_result);
        
        if heuristically_clean(&bboard, kind) {
            //if it's allshortcut, display the board
            /*match &bfs_result {
                BfsResult::AllShortcut => {
                    println!("AllShortcut heuristically clean board");
                    print!("{}", format_board_header(&bboard, exit, "Heuristically clean board"));
                    print!("{}", format_board(&bboard, exit));
                    println!("Expected path:");
                    for dir in &next_path {
                        match dir {
                            Dir::Up => print!("U"),
                            Dir::Down => print!("D"),
                            Dir::Left => print!("L"),
                            Dir::Right => print!("R"),
                        }
                    }
                }
                BfsResult::Unreachable => {}
                BfsResult::SomeClean { .. } => {}
                BfsResult::Indeterminate { .. } => {}
            }*/
            shared_guard.heuristic_stats.record_match(&bfs_result);
        }

        if elapsed_ms >= shared_guard.slowest_millis && elapsed_ms > 10 {
            shared_guard.slowest_millis = elapsed_ms;
            println!(
                "Warning: forward BFS took {:?} at depth {}.",
                elapsed,
                depth + 1
            );
            print!("{}", format_board_header(&bboard, exit, "Slow BFS board"));
            print!("{}", format_board(&bboard, exit));
            match &bfs_result {
                BfsResult::Unreachable => println!("BFS result: Unreachable"),
                BfsResult::AllShortcut => println!("BFS result: AllShortcut"),
                BfsResult::SomeClean { .. } => println!("BFS result: SomeClean"),
                BfsResult::Indeterminate { .. } => println!("BFS result: Indeterminate"),
            }
            println!("Expected path:");
            for dir in &next_path {
                match dir {
                    Dir::Up => print!("U"),
                    Dir::Down => print!("D"),
                    Dir::Left => print!("L"),
                    Dir::Right => print!("R"),
                }
            }
            println!();
        }
    }

    match bfs_result {
        BfsResult::Unreachable => {
            println!("Found unreachable board at depth {}:", depth + 1);
            print!("{}", format_board_header(&bboard, exit, "Unreachable board"));
            print!("{}", format_board(&bboard, exit));
            forward_bfs(&bboard, exit, target_steps, &next_path, &[], false, 1, None);
            panic!("Unreachable board found! This should not happen, as the backward move should be valid. Please investigate.");
        }
        BfsResult::AllShortcut => {
            let player = board.find_player();
            let (px, py) = board.coords(player);
            let wall_pos = match dir.apply(px, py) {
                Some((fx, fy)) if board.in_inner(fx, fy) => Some(board.flat(fx, fy)),
                _ => None,
            };
            
            if let Some(pos) = wall_pos {
                if board.get_flat(pos) == Tile::Wall && pos < 128 {
                    *required_walls |= 1u128 << pos;
                }
            }
        }
        BfsResult::Indeterminate { .. } => unreachable!("complete child solve returned Indeterminate"),
        BfsResult::SomeClean {
            ruled_out,
            shortcuts: _,
            publishable,
        } => {
            if publishable && target_steps <= step_limit_to_print_clean {
                println!("Found some-clean board at depth {}:", depth + 1);
                print!("{}", format_board_header(&bboard, exit, "Some-clean board"));
                print!("{}", format_board(&bboard, exit));
                println!("Number of shortcut paths retained: {}", shortcuts.paths().len());
                println!("Ruled out: {} positions", ruled_out.len());
            }
            let mut rboard = bboard;
            // With no exclusions this is the identical normalized board just
            // solved above. Reuse its publication proof for the record check.
            let verified_clean = ruled_out.is_empty().then_some(publishable);
            apply_ruled_out(&mut rboard, &ruled_out);
            rboard.collapse_single_tile_qbs();
            rboard.compact_blocks();
            backward_dfs_recursive(
                &rboard,
                depth + 1,
                exit,
                shared,
                Some(dir),
                next_path,
                shortcuts,
                child_required_walls,
                verified_clean,
            );
        }
    }
}

pub fn backward_dfs(goal_x: u16, goal_y: u16, inner_width: u16, inner_height: u16) {
    backward_dfs_with_options(goal_x, goal_y, inner_width, inner_height, &BackwardSearchOptions::default())
        .expect("backward search failed");
}

fn start_reporter(shared: &Arc<SearchRuntime>, worker: usize, log: Option<SearchLog>) -> ProgressReporter {
    let progress_shared = Arc::clone(shared);
    ProgressReporter::start(REPORT_INTERVAL, move || {
        let state = progress_shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let message = state.progress_message_for(worker, "periodic");
        // Append the snapshot before another subtree can update this cursor.
        if let Some(log) = &log { let _ = log.line(&message); String::new() } else { message }
    }, |message| {
        if !message.is_empty() { print_progress(message); }
    })
}

fn backward_moves(path: &[Dir]) -> String {
    path.iter().rev().map(|dir| match dir.opposite() {
        Dir::Up => 'U', Dir::Down => 'D', Dir::Left => 'L', Dir::Right => 'R',
    }).collect()
}

fn run_worker(shared: Arc<SearchRuntime>, worker: usize, log: SearchLog) {
    let _routing = search_log::enter(worker, log.clone());
    let queue = shared.queue.as_ref().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while let Some(job) = queue.pop() {
            if shared.stopping.load(Ordering::Acquire) { break; }
            let checkpoint_key = shared.checkpoint.as_ref().map(|_| {
                crate::checkpoint::subtree_key(&job.board, &job.path, job.required_walls)
            });
            let prefix = backward_moves(&job.path);
            if let Some(records) = &shared.records {
                records.begin_root(shared.checkpoint.as_ref(), job.id, prefix.clone())
                    .expect("failed to start subtree records");
            }
            shared.state.lock().unwrap().progress[worker].update(&job.path, "subtree");
            println!("[subtree:start] id={} depth={} moves={prefix}", job.id, job.depth);
            backward_dfs_recursive(&job.board, job.depth, job.exit, &shared,
                job.prev_undo_dir, job.path, job.shortcuts, job.required_walls, None);
            if queue.is_cancelled() {
                println!("[subtree:cancelled] id={} moves={prefix}", job.id);
                break;
            }
            if let Some(records) = &shared.records {
                records.finish_root(shared.checkpoint.as_ref()).expect("failed to finish subtree records");
            }
            if let Some(key) = checkpoint_key {
                shared.checkpoint.as_ref().unwrap().lock().unwrap().complete(key)
                    .expect("failed to checkpoint completed subtree");
            }
            {
                let mut state = shared.state.lock().unwrap();
                state.subtrees_completed += 1;
                state.progress[worker].set_phase("idle");
            }
            println!("[subtree:finished] id={} moves={prefix}", job.id);
        }
    }));
    if let Err(panic) = result {
        queue.cancel();
        let message = panic.downcast_ref::<String>().map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("unknown panic");
        let _ = log.line(&format!("[worker:failed] {message}"));
        std::panic::resume_unwind(panic);
    }
}

pub fn backward_dfs_with_options(
    goal_x: u16, goal_y: u16, inner_width: u16, inner_height: u16,
    options: &BackwardSearchOptions,
) -> std::io::Result<BackwardSearchSummary> {
    use std::io::{Error, ErrorKind};
    if options.threads == 0 || options.split_depth == 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "threads and split depth must be positive"));
    }
    if options.resume && options.checkpoint_dir.is_none() {
        return Err(Error::new(ErrorKind::InvalidInput, "resume requires a checkpoint directory"));
    }
    if inner_width == 0 || inner_height == 0
        || (u64::from(inner_width) + 2) * (u64::from(inner_height) + 2) > crate::board::MAX_TILES as u64
        || goal_x == 0 || goal_y == 0 || goal_x > inner_width || goal_y > inner_height {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid board dimensions or exit"));
    }
    let record_name = format!("longest_levels_{inner_width}_{inner_height}.txt");
    let checkpoint = if let Some(directory) = &options.checkpoint_dir {
        let metadata = format!("quantum-checkpoint-v1/search-v1/{inner_width}/{inner_height}/{goal_x}/{goal_y}/{}/{}/{}",
            options.split_depth, options.symmetry,
            cfg!(feature = "shortcut-reuse"));
        Some(crate::checkpoint::Checkpoint::open(directory, &metadata, options.resume, &record_name)?)
    } else { None };
    let checkpoint_best = checkpoint.as_ref().map_or(0, |c| c.best);
    let checkpoint_directory = checkpoint.as_ref().map(|c| c.directory.clone());
    if let Some(checkpoint) = &checkpoint {
        std::println!("Checkpoint: {} ({} completed subtrees, record {})",
            checkpoint.directory.display(), checkpoint.completed_count(), checkpoint.best);
    }
    let parallel = options.threads > 1 || checkpoint.is_some();
    let mut logs = Vec::new();
    let log_directory = if parallel {
        std::fs::create_dir_all(&options.log_dir)?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(Error::other)?.as_nanos();
        let directory = options.log_dir.join(format!("{inner_width}x{inner_height}-exit-{goal_x}-{goal_y}-{stamp}-{}", std::process::id()));
        std::fs::create_dir(&directory)?;
        logs.push(SearchLog::create(&directory.join("coordinator.log"))?);
        for worker in 1..=options.threads {
            logs.push(SearchLog::create(&directory.join(format!("worker-{worker:02}.log")))?);
        }
        let directory = if directory.is_absolute() { directory } else { std::env::current_dir()?.join(directory) };
        std::println!("Parallel search: {} workers, split depth {}, logs: {}",
            options.threads, options.split_depth, directory.display());
        Some(directory)
    } else { None };
    std::fs::create_dir_all(&options.record_dir)?;
    let board = QuantumBoard::wall_filled_with_player(inner_width, inner_height, goal_x, goal_y);
    let exit = board.flat(goal_x, goal_y);
    let mut initial_state = SharedState::new(inner_width, inner_height, (goal_x, goal_y),
        if parallel { options.threads + 1 } else { 1 });
    initial_state.longest_found = checkpoint_best;
    let records_directory = if let Some(directory) = &checkpoint_directory {
        directory.join("search-records")
    } else if let Some(directory) = &log_directory {
        directory.join("search-records")
    } else {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(Error::other)?.as_nanos();
        options.record_dir.join(format!("search-records-{inner_width}x{inner_height}-exit-{goal_x}-{goal_y}-{stamp}"))
    };
    let records = if options.extra_records {
        let records = crate::search_records::SearchRecords::new(records_directory, checkpoint.as_ref())?;
        std::println!("Search records: {}", records.directory.display());
        Some(records)
    } else { None };
    let shared = Arc::new(SearchRuntime {
        state: Mutex::new(initial_state),
        queue: parallel.then(|| WorkQueue::new(options.threads.saturating_mul(2))),
        split_depth: options.split_depth,
        record_path: checkpoint_directory.as_ref().unwrap_or(&options.record_dir).join(record_name),
        frontier_seen: checkpoint.as_ref().map(|_| Mutex::new(FxHashMap::default())),
        checkpoint: checkpoint.map(Mutex::new),
        symmetries: if options.symmetry { crate::symmetry::exit_symmetries(inner_width, inner_height, goal_x, goal_y) } else { Vec::new() },
        stopping: AtomicBool::new(false),
        checkpoint_skips: std::sync::atomic::AtomicUsize::new(0),
        records,
    });
    std::println!("Exit-preserving symmetries: {}", shared.symmetries.len());
    let stop_reporter = checkpoint_directory.map(|directory| {
        let runtime = Arc::clone(&shared);
        ProgressReporter::start(std::time::Duration::from_millis(250), move || {
            if directory.join("STOP").exists() && !runtime.stopping.swap(true, Ordering::AcqRel) {
                runtime.queue.as_ref().unwrap().discard_pending();
                std::println!("Stop requested: finishing active subtrees and saving their completion.");
            }
            String::new()
        }, |_| {})
    });
    let reporters: Vec<_> = if parallel {
        logs.iter().enumerate().map(|(id, log)| start_reporter(&shared, id, Some(log.clone()))).collect()
    } else { vec![start_reporter(&shared, 0, None)] };
    let routing = logs.first().map(|log| search_log::enter(0, log.clone()));
    let result = if parallel {
        let queue = shared.queue.as_ref().unwrap();
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            let mut spawn_error = None;
            for (worker, log) in logs.iter().enumerate().skip(1) {
                let runtime = Arc::clone(&shared);
                let log = log.clone();
                match std::thread::Builder::new().name(format!("search-{worker:02}"))
                    .spawn_scoped(scope, move || run_worker(runtime, worker, log)) {
                    Ok(handle) => workers.push(handle),
                    Err(error) => { spawn_error = Some(error); queue.cancel(); break; }
                }
            }
            let coordinator = if spawn_error.is_none() {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    backward_dfs_recursive(&board, 0, exit, &shared, None, vec![], ShortcutPaths::default(), 0, None);
                }))
            } else { Ok(()) };
            if coordinator.is_err() { queue.cancel(); } else { queue.finish(); }
            shared.state.lock().unwrap_or_else(|e| e.into_inner()).progress[0].set_phase("waiting-workers");
            let mut failure = coordinator.err();
            for worker in workers {
                if let Err(panic) = worker.join() { if failure.is_none() { failure = Some(panic); } }
            }
            if let Some(panic) = failure { std::panic::resume_unwind(panic); }
            match spawn_error { Some(error) => Err(error), None => Ok(()) }
        })
    } else {
        if let Some(records) = &shared.records { records.begin_root(None, 0, "-".to_owned())?; }
        backward_dfs_recursive(&board, 0, exit, &shared, None, vec![], ShortcutPaths::default(), 0, None);
        if let Some(records) = &shared.records { records.finish_root(None)?; }
        Ok(())
    };
    drop(reporters);
    drop(stop_reporter);
    drop(routing);
    result?;
    let state = shared.state.lock().unwrap();
    for (worker, log) in logs.iter().enumerate() {
        log.line(&state.progress_message_for(worker, "finished"))?;
    }
    print_progress(state.progress_message_for(0, "finished"));
    if parallel {
        std::println!("Parallel search finished: workers={} subtrees={} boards={} best={}",
            options.threads, state.subtrees_completed, state.seen.len(), state.longest_found);
    }
    state.timing.print();
    println!("Frozen-exit subtrees pruned: {}", state.frozen_exit_prunes);
    println!("Symmetric subtrees pruned: {}", state.symmetry_prunes);
    let stopped = shared.stopping.load(Ordering::Acquire);
    let checkpoint_skips = shared.checkpoint_skips.load(Ordering::Relaxed);
    if shared.checkpoint.is_some() {
        std::println!("Checkpoint: skipped {checkpoint_skips} saved subtrees; {}",
            if stopped { "stopped; resume with --resume" } else { "search complete" });
    }
    state.heuristic_stats.print();
    Ok(BackwardSearchSummary {
        longest_found: state.longest_found, unique_boards: state.seen.len(),
        subtrees_completed: state.subtrees_completed, log_directory, stopped, checkpoint_skips,
    })
}





/// Simulates the forward path step-by-step from the parent player's position
/// and returns the first index where the player is about to interact with a block, QB, or the exit.
fn find_first_interaction_source(board: &QuantumBoard, path: &[Dir], exit: FlatIdx) -> Option<FlatIdx> {
    let mut current_pos = board.find_player();
    
    for &dir in path {
        let (cx, cy) = board.coords(current_pos);
        if let Some((nx, ny)) = dir.apply(cx, cy) {
            if board.in_inner(nx, ny) {
                let next_idx = board.flat(nx, ny);
                let next_tile = board.get_flat(next_idx);
                
                // Interaction occurs if the tile is the exit, a Box, or a Quantum Block
                if next_idx == exit || matches!(next_tile, Tile::Box | Tile::Quantum(_)) {
                    return Some(current_pos);
                }
                current_pos = next_idx;
            }
        }
    }
    None
}

/// Computes the exact shortest path distance from a target tile to all reachable tiles,
/// moving strictly through `Tile::Floor` spaces on the parent board.
fn compute_floor_distances(board: &QuantumBoard, target: FlatIdx) -> Vec<Option<u32>> {
    let total_cells = (board.width * board.height) as usize;
    let mut distances = vec![None; total_cells];
    let mut queue = VecDeque::new();

    distances[target as usize] = Some(0);
    queue.push_back(target);

    while let Some(curr) = queue.pop_front() {
        let curr_dist = distances[curr as usize].unwrap();
        let (cx, cy) = board.coords(curr);

        for dir in Dir::all() {
            if let Some((nx, ny)) = dir.apply(cx, cy) {
                if board.in_inner(nx, ny) {
                    let next_idx = board.flat(nx, ny);
                    // The tile must be completely open floor (and not already visited)
                    if (board.get_flat(next_idx) == Tile::Floor || board.get_flat(next_idx) == Tile::Player) && distances[next_idx as usize].is_none() {
                        distances[next_idx as usize] = Some(curr_dist + 1);
                        queue.push_back(next_idx);
                    }
                }
            }
        }
    }

    distances
}

#[cfg(test)]
mod worker_failure_tests {
    use super::*;

    #[test]
    fn a_panicking_worker_cancels_queued_work_and_reports_the_failure() {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scratch/tests-worker-failure")
            .join(format!("{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let log_path = directory.join("worker.log");
        let runtime = Arc::new(SearchRuntime {
            state: Mutex::new(SharedState::new(4, 4, (2, 2), 2)),
            queue: Some(WorkQueue::new(1)), split_depth: 10,
            record_path: directory.join("records.txt"),
            checkpoint: None, frontier_seen: None, symmetries: Vec::new(),
            stopping: AtomicBool::new(false), checkpoint_skips: std::sync::atomic::AtomicUsize::new(0),
            records: None,
        });
        // A deliberately invalid board fails inside the forward solve.
        runtime.queue.as_ref().unwrap().push(BackwardJob {
            id: 1, board: QuantumBoard::new(4, 4), depth: 10, exit: 14,
            prev_undo_dir: None, path: vec![Dir::Up; 10],
            shortcuts: ShortcutPaths::default(), required_walls: 0,
        });
        let shared = Arc::clone(&runtime);
        let log = SearchLog::create(&log_path).unwrap();
        assert!(std::thread::spawn(move || run_worker(shared, 1, log)).join().is_err());
        assert!(runtime.queue.as_ref().unwrap().is_cancelled());
        assert!(runtime.queue.as_ref().unwrap().pop().is_none());
        assert!(std::fs::read_to_string(log_path).unwrap().contains("[worker:failed]"));
    }
}
