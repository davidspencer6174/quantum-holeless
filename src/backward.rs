// backward.rs — generate all boards reachable via one "undo" move.

use rustc_hash::FxHashMap;
use rustc_hash::FxHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use crate::bfs::{forward_bfs, BfsResult};
use crate::board::QuantumBoard;
use crate::io::{format_board, format_board_header};
use crate::qstate::Shortcut;
use crate::types::{fis_single, Dir, FlatIdx, FlatIdxSet, QBlockId, QuantumBlock, Tile};
use std::fs::OpenOptions;
use std::io::Write;

// ── Board fingerprint ─────────────────────────────────────────────────────────

fn board_fingerprint(board: &QuantumBoard) -> u64 {
    let mut h = FxHasher::default();
    board.width.hash(&mut h);
    board.height.hash(&mut h);

    let total = (board.width * board.height) as usize;

    // 1. Map old IDs to canonical IDs based on row-major appearance order.
    let mut old_to_canonical = FxHashMap::default();
    let mut appearance_order = Vec::new();
    let mut next_canonical = 0u8;

    for (idx, &tile) in board.tiles[..total].iter().enumerate() {
        if let Tile::Quantum(old_id) = tile {
            old_to_canonical.entry(old_id).or_insert_with(|| {
                appearance_order.push(old_id);
                let can = next_canonical;
                next_canonical += 1;
                can
            });
        }
    }

    // 2. Hash tiles using canonical IDs.
    for &tile in &board.tiles[..total] {
        match tile {
            Tile::Quantum(id) => Tile::Quantum(old_to_canonical[&id]).hash(&mut h),
            other => other.hash(&mut h),
        }
    }

    // 3. Hash QuantumBlock data in the stable appearance order.
    for &old_id in &appearance_order {
        let qb = &board.quantum_blocks[old_id as usize];
        let mut cells: Vec<(FlatIdx, &FlatIdxSet)> = qb
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
    // For each concrete box, check whether it can potentially be pushed into
    // the dead-end tile at p_idx under permissive movement assumptions.
    for idx in 0..(board.width * board.height) {
        let idx = idx as FlatIdx;
        if board.get_flat(idx) == Tile::Box {
            let push_mask = board.get_box_push_mask(idx);
            if push_mask != 0 && block_domain_analysis(board, p_idx, idx, push_mask, exit) {
                return true;
            }
        }
    }

    // For each quantum block position, treat any current position that can
    // represent a box as a potential source and search its domain.
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
// PureMove: no block was pulled. May be a shortcut — see below.
//
// Pull { qb_id, pulled_pos }: a block landed at pulled_pos (the player's old
//   position). If the block started at pulled_pos the push/pull cancel — a
//   shortcut. We remove pulled_pos from the QB before BFS.

#[derive(Debug, Clone, Copy)]
enum UndoKind {
    PureMove,
    Pull {
        qb_id: QBlockId,
        pulled_pos: FlatIdx,
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
            // Idea 1: Respect the required_walls mask from previous shortcut discoveries
            if (required_walls & (1u128 << from)) != 0 {
                //println!("Refusing to delete wall at index {} due to required_walls mask", from);
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
        (None, _) | (_, Some(Tile::Floor)) => UndoKind::PureMove,

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
            }
        }

        (Some(_src), Some(Tile::Quantum(push_id))) => {
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
            }
        }

        (_, Some(Tile::Player)) | (_, None) => UndoKind::PureMove,
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

// ── Backward DFS ──────────────────────────────────────────────────────────────

// ── BFS timing histogram ──────────────────────────────────────────────────────

struct BfsTimingStats {
    lt1ms: u64,   // < 1ms
    lt10ms: u64,  // 1ms – 10ms
    lt100ms: u64, // 10ms – 100ms
    lt1s: u64,    // 100ms – 1s
    lt10s: u64,   // 1s – 10s
    ge10s: u64,   // ≥ 10s
    total: u64,
    allshortcut: u128, // time of forward BFS calls that returned AllShortcut
    someclean: u128,   // time of forward BFS calls that returned SomeClean
    indeterminate: u128, // time of forward BFS calls that returned Indeterminate
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
        println!(
            "  1–10ms:     {:>8}  ({:.1}%)",
            self.lt10ms,
            pct(self.lt10ms)
        );
        println!(
            "  10–100ms:   {:>8}  ({:.1}%)",
            self.lt100ms,
            pct(self.lt100ms)
        );
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

struct SharedState {
    seen: FxHashMap<u64, usize>,
    longest_found: usize,
    slowest_millis: u64,
    timing: BfsTimingStats,
}

impl SharedState {
    fn new() -> Self {
        Self {
            seen: FxHashMap::default(),
            longest_found: 0,
            slowest_millis: 0,
            timing: BfsTimingStats::new(),
        }
    }
}

const PARALLEL_DEPTH_THRESHOLD: usize = 8;

// ── Backward DFS ──────────────────────────────────────────────────────────────

fn backward_dfs_recursive(
    board: &QuantumBoard,
    depth: usize,
    exit: FlatIdx,
    shared: &Arc<Mutex<SharedState>>,
    prev_undo_dir: Option<Dir>,
    path: Vec<Dir>,
    shortcuts: Vec<Shortcut>,
    required_walls: u128,
) {
    //if shortcuts.len() > 0 {
    //print board and shortcuts vec
    //    print!("{}", format_board_header(board, exit, "Current board"));
    //    print!("{}", format_board(board, exit));
    //    print!("Shortcuts: {:?}\n", shortcuts);
    //}
    //print!("{}", format_board_header(board, exit, "Current board"));
    //print!("{}", format_board(board, exit));

    let step_limit_to_print_clean = 2;

    let fp = board_fingerprint(board);
    let mut should_write_record = false;
    let mut record_candidate = false;
    {
        let mut shared_guard = shared.lock().unwrap();
        match shared_guard.seen.get(&fp).copied() {
            Some(prev_depth) if prev_depth >= depth => return,
            _ => {
                shared_guard.seen.insert(fp, depth);
            }
        }

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

        if depth > shared_guard.longest_found {
            //let exit_tile = board.get_flat(exit);
            //let trivially_covered = matches!(exit_tile, Tile::Box)
            //    || matches!(exit_tile, Tile::Quantum(qid)
            //        if board.quantum_blocks[qid as usize].num_positions() == 1);
            //if !trivially_covered {
                record_candidate = true;
            //}
        }
    }

    if record_candidate {
        let start_time = std::time::Instant::now();
        let bfs_result = forward_bfs(board, exit, depth as u32, &path,
            &[],//&shortcuts,
             false, 0, None);
        let elapsed = start_time.elapsed();
        let elapsed_us = elapsed.as_micros();
        let elapsed_ms = elapsed.as_millis() as u64;

        {
            let mut shared_guard = shared.lock().unwrap();
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
            BfsResult::SomeClean { .. } => {
                //println!("Found some-clean board at depth {}:", depth);
                //print!("{}", format_board_header(board, exit, "Some-clean board"));
                //print!("{}", format_board(board, exit));

                let mut shared_guard = shared.lock().unwrap();
                if depth > shared_guard.longest_found {
                    //check if it's possible for the exit to be uncovered
                    let exit_tile = board.get_flat(exit);
                    let trivially_covered = matches!(exit_tile, Tile::Box)
                        || matches!(exit_tile, Tile::Quantum(qid)
                            if board.quantum_blocks[qid as usize].num_positions() == 1);
                    if !trivially_covered {
                        shared_guard.longest_found = depth;
                        should_write_record = true;
                    }
                }
            }
            BfsResult::Indeterminate { .. } => unreachable!("full forward_bfs returned Indeterminate"),
        }
    }

    if should_write_record {
        let header = format_board_header(
            &board,
            exit,
            &format!("Longest path so far with depth = {}", depth),
        );
        let board_str = format_board(&board, exit);

        println!(
            ">>> New record found at depth {}! Writing to longest_levels_pp1_{}_{}.txt",
            depth,
            board.width - 2,
            board.height - 2
        );

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("longest_levels_{}_{}.txt", board.width - 2, board.height - 2))
        {
            let _ = write!(file, "{}{}\n", header, board_str);
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

    if depth < PARALLEL_DEPTH_THRESHOLD {
        // Parallelize search above the threshold as well so idle Rayon workers
        // can help find more threshold-root subtrees.
        let mut local_required_walls = required_walls;
        rayon::scope(|s| {
            let mut children_iter = children.into_iter();
            if let Some((first_dir, first_board, first_kind)) = children_iter.next() {
                for (dir, bboard, kind) in children_iter {
                    let board_ref = board;
                    let shared_ref = Arc::clone(shared);
                    let path_clone = path.clone();
                    let shortcuts_clone = shortcuts.clone();
                    let mut thread_required_walls = local_required_walls;
                    s.spawn(move |_| {
                        process_child(
                            depth,
                            board_ref,
                            exit,
                            &shared_ref,
                            prev_undo_dir,
                            path_clone,
                            shortcuts_clone,
                            &mut thread_required_walls,
                            dir,
                            bboard,
                            kind,
                            step_limit_to_print_clean,
                        );
                    });
                }

                process_child(
                    depth,
                    board,
                    exit,
                    shared,
                    prev_undo_dir,
                    path,
                    shortcuts,
                    &mut local_required_walls,
                    first_dir,
                    first_board,
                    first_kind,
                    step_limit_to_print_clean,
                );
            }
        });
    } else {
        // Sequential processing inside an already assigned subtree.
        let mut local_required_walls = required_walls;
        for (dir, bboard, kind) in children {
            process_child(
                depth,
                board,
                exit,
                shared,
                prev_undo_dir,
                path.clone(),
                shortcuts.clone(),
                &mut local_required_walls,
                dir,
                bboard,
                kind,
                step_limit_to_print_clean,
            );
        }
    }
}

fn process_child(
    depth: usize,
    board: &QuantumBoard,
    exit: FlatIdx,
    shared: &Arc<Mutex<SharedState>>,
    prev_undo_dir: Option<Dir>,
    path: Vec<Dir>,
    shortcuts: Vec<Shortcut>,
    required_walls: &mut u128,
    dir: Dir,
    mut bboard: QuantumBoard,
    kind: UndoKind,
    step_limit_to_print_clean: u32,
) {
    let child_required_walls = *required_walls;
    if matches!(kind, UndoKind::PureMove) {
        let is_shortcut = depth > 0 && prev_undo_dir == Some(dir.opposite());
        if is_shortcut {
            return;
        }
    }

    if let UndoKind::Pull { qb_id, pulled_pos } = kind {
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

    let mut next_path = vec![dir.opposite()];
    next_path.extend(path.iter().copied());

    let target_steps = (depth + 1) as u32;

    let p_idx = bboard.find_player();
    if board.get_flat(p_idx) == Tile::Wall && is_dead_end(&bboard, p_idx) {
        if !can_block_reach_dead_end(&bboard, p_idx, exit) {
            if target_steps <= step_limit_to_print_clean {
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
            );
            return;
        }
    }

    //let use_quick_check = {
    //    let shared_guard = shared.lock().unwrap();
    //    let is_record_candidate = depth + 1 > shared_guard.longest_found;
    //    depth > 20 && !is_record_candidate
    //};
    //let mut verbose_special = 0;
    //if depth == 5 {
    //    verbose_special = 1;
    //}
    let use_quick_check = false;
    let start_time = std::time::Instant::now();
    let bfs_result = forward_bfs(
        &bboard,
        exit,
        target_steps,
        &next_path,
        &[],//&shortcuts,
        use_quick_check,
        0,
        Some(10_000),//Some(35_000_000),
    );
    let elapsed = start_time.elapsed();
    let elapsed_us = elapsed.as_micros();
    let elapsed_ms = elapsed.as_millis() as u64;

    {
        let mut shared_guard = shared.lock().unwrap();
        shared_guard.timing.record(elapsed_us, &bfs_result);
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
        }
    }

    match bfs_result {
        BfsResult::Unreachable => {
            println!("Found unreachable board at depth {}:", depth + 1);
            print!("{}", format_board_header(&bboard, exit, "Unreachable board"));
            print!("{}", format_board(&bboard, exit));
            forward_bfs(&bboard, exit, target_steps, &next_path,
                &[],//&shortcuts,
                 false, 1, None);
            panic!("Unreachable board found! This should not happen, as the backward move should be valid. Please investigate.");
        }
        BfsResult::AllShortcut => {
            // If the undo move removed a wall, that wall must be present in all descendants
            let player = board.find_player();
            let (px, py) = board.coords(player);
            let wall_pos = match dir.apply(px, py) {
                Some((fx, fy)) if board.in_inner(fx, fy) => Some(board.flat(fx, fy)),
                _ => None,
            };
            
            if let Some(pos) = wall_pos {
                if board.get_flat(pos) == Tile::Wall && pos < 128 {
                    // Mark this wall as required for all future exploration
                    *required_walls |= 1u128 << pos;
                }
            }
        }
        BfsResult::Indeterminate {
            ruled_out,
            shortcuts: next_shortcuts,
        } => {
            /*
            //if target_steps <= step_limit_to_print_clean {
                println!("Found indeterminate board at depth {}:", depth + 1);
                print!("{}", format_board_header(&bboard, exit, "Indeterminate board"));
                print!("{}", format_board(&bboard, exit));
            //}
            let mut rboard = bboard;
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
                next_shortcuts,
                child_required_walls,
            );*/
        }
        BfsResult::SomeClean {
            ruled_out,
            shortcuts: next_shortcuts,
        } => {
            if target_steps <= step_limit_to_print_clean {
                println!("Found some-clean board at depth {}:", depth + 1);
                print!("{}", format_board_header(&bboard, exit, "Some-clean board"));
                print!("{}", format_board(&bboard, exit));
                println!("Number of shortcuts found: {}", next_shortcuts.len());
                println!("Ruled out: {} positions", ruled_out.len());
            }
            let mut rboard = bboard;
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
                next_shortcuts,
                child_required_walls,
            );
        }
    }
}

pub fn backward_dfs(goal_x: u16, goal_y: u16, inner_width: u16, inner_height: u16) {
    let board = QuantumBoard::wall_filled_with_player(inner_width, inner_height, goal_x, goal_y);
    let exit = board.flat(goal_x, goal_y);
    let shared = Arc::new(Mutex::new(SharedState::new()));
    backward_dfs_recursive(&board, 0, exit, &shared, None, vec![], vec![], 0);
    shared.lock().unwrap().timing.print();
}
