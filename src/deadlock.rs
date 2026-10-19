use crate::board::QuantumBoard;
use crate::qstate::{slot_mask_at, QState, INVALID};
use crate::types::{Dir, FlatIdx, Tile};
use smallvec::SmallVec;

// ── BoxState ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct BoxState {
    pub moved: SmallVec<[(FlatIdx, FlatIdx); 4]>,
}

impl BoxState {
    pub fn origin_groups(board: &QuantumBoard) -> [u128; 16] {
        let mut groups = [0; 16];
        for (pos, &tile) in board.tiles.iter().enumerate() {
            if tile == Tile::Box { groups[board.get_box_push_mask(pos as FlatIdx) as usize] |= 1u128 << pos; }
        }
        groups
    }

    /// Concrete boxes with the same push mask are interchangeable within a
    /// forward solve. Assign sorted destinations to sorted origins so their
    /// permutations share one visited entry. QB slot identities are unchanged.
    pub fn canonicalize_group(&mut self, origins: u128) {
        if origins.count_ones() < 2 { return; }
        let mut positions = SmallVec::<[FlatIdx; 16]>::new();
        let mut remaining = origins;
        while remaining != 0 {
            let orig = remaining.trailing_zeros() as FlatIdx;
            remaining &= remaining - 1;
            positions.push(self.current_of(orig));
        }
        positions.sort_unstable();
        self.moved.retain(|entry| origins & (1u128 << entry.0) == 0);
        let mut remaining = origins;
        for current in positions {
            let orig = remaining.trailing_zeros() as FlatIdx;
            remaining &= remaining - 1;
            if orig != current { self.moved.push((orig, current)); }
        }
        self.moved.sort_unstable_by_key(|&(orig, _)| orig);
    }

    pub fn current_of(&self, orig: FlatIdx) -> FlatIdx {
        self.moved
            .iter()
            .find(|&&(o, _)| o == orig)
            .map_or(orig, |&(_, c)| c)
    }

    pub fn move_box(&mut self, orig: FlatIdx, dest: FlatIdx) {
        match self.moved.binary_search_by_key(&orig, |&(o, _)| o) {
            Ok(i) if orig == dest => { self.moved.remove(i); }
            Ok(i) => self.moved[i].1 = dest,
            Err(i) if orig != dest => self.moved.insert(i, (orig, dest)),
            Err(_) => {}
        }
    }

    pub fn occupies(&self, pos: FlatIdx, board: &QuantumBoard) -> bool {
        if self.moved.iter().any(|&(_, dest)| dest == pos) {
            return true;
        }
        if board.get_flat(pos) == Tile::Box {
            if !self.moved.iter().any(|&(orig, _)| orig == pos) {
                return true;
            }
        }
        false
    }

    pub fn orig_at(&self, pos: FlatIdx, board: &QuantumBoard) -> Option<FlatIdx> {
        if let Some(&(orig, _)) = self.moved.iter().find(|&&(_, dest)| dest == pos) {
            return Some(orig);
        }
        if board.get_flat(pos) == Tile::Box && !self.moved.iter().any(|&(orig, _)| orig == pos) {
            return Some(pos);
        }
        None
    }
}

#[cfg(test)]
mod box_state_tests {
    use super::*;

    #[test]
    fn equal_mask_boxes_are_interchangeable_including_deleted_boxes() {
        let mut board = QuantumBoard::new(4, 4);
        let origins = [board.flat(1, 1), board.flat(2, 1), board.flat(3, 1)];
        for (i, &orig) in origins.iter().enumerate() {
            board.set_flat(orig, Tile::Box);
            board.set_box_push_mask(orig, if i < 2 { 3 } else { 1 });
        }
        let groups = BoxState::origin_groups(&board);
        for destinations in [[20, 21], [origins[0], origins[1]], [21, crate::qstate::FREE_SLOT as FlatIdx]] {
            let mut a = BoxState::default();
            let mut b = BoxState::default();
            for i in 0..2 {
                a.move_box(origins[i], destinations[i]);
                b.move_box(origins[i], destinations[1 - i]);
            }
            a.move_box(origins[2], 22);
            b.move_box(origins[2], 22);
            a.canonicalize_group(groups[3]);
            b.canonicalize_group(groups[3]);
            assert_eq!(a, b);
            assert_eq!(a.current_of(origins[2]), 22);
            let saved = a.clone();
            a.canonicalize_group(groups[3]);
            assert_eq!(a, saved);
        }
    }

    #[test]
    fn returning_home_restores_the_same_state_and_preserves_other_boxes() {
        let mut board = QuantumBoard::new(4, 4);
        let a = board.flat(2, 2);
        let b = board.flat(3, 3);
        board.set_flat(a, Tile::Box);
        board.set_flat(b, Tile::Box);
        let mut state = BoxState::default();
        state.move_box(b, b - 1);
        state.move_box(a, a + 1);
        assert_eq!(state.moved.as_slice(), &[(a, a + 1), (b, b - 1)]);
        state.move_box(a, a);
        assert_eq!(state.moved.as_slice(), &[(b, b - 1)]);
        assert_eq!(state.orig_at(a, &board), Some(a));
        assert_eq!(state.orig_at(a + 1, &board), None);
        state.move_box(b, b);
        state.move_box(a, a);
        assert_eq!(state, BoxState::default());
    }
}

fn get_reachable_mask(
    board: &QuantumBoard,
    start_pos: FlatIdx,
    obstacles: u128,
    stack: &mut Vec<FlatIdx>,
) -> u128 {
    let mut reachable = 1u128 << start_pos;
    stack.clear();
    stack.push(start_pos);
    while let Some(curr) = stack.pop() {
        let (x, y) = board.coords(curr);
        for dir in Dir::all() {
            if let Some((nx, ny)) = dir.apply(x, y) {
                if board.in_inner(nx, ny) {
                    let next = board.flat(nx, ny);
                    let bit = 1u128 << next;
                    if (obstacles & bit) == 0 && (reachable & bit) == 0 {
                        reachable |= bit;
                        stack.push(next);
                    }
                }
            }
        }
    }
    reachable
}

pub fn effective_tile(pos: FlatIdx, board: &QuantumBoard, box_state: &BoxState) -> Tile {
    if box_state.occupies(pos, board) {
        return Tile::Box;
    }
    match board.get_flat(pos) {
        Tile::Box => Tile::Floor,
        t => t,
    }
}

pub fn corner_unreachable_flowchart(
    board: &QuantumBoard,
    qstate: &QState,
    qb_masks: &[u64],
    player_pos: FlatIdx,
    exit: FlatIdx,
    static_walls: u128,
    initial_covered_mask: u128,
    reachable_stack: &mut Vec<FlatIdx>,
    idx: FlatIdx,
    inward_direction_1: Dir,
    inward_direction_2: Dir,
) -> bool {
    if player_pos == idx{
        return false;
    }
    let neighbor_1 = inward_direction_1.apply(board.coords(idx).0, board.coords(idx).1).unwrap();
    let neighbor_2 = inward_direction_2.apply(board.coords(idx).0, board.coords(idx).1).unwrap();
    if initial_covered_mask & (1u128 << board.flat(neighbor_1.0, neighbor_1.1)) != 0{
        if board.flat(neighbor_2.0, neighbor_2.1) == exit { return true; }
        let mut edge = neighbor_2;
        let mut inside = inward_direction_1.apply(neighbor_2.0, neighbor_2.1).unwrap();
        loop {
            //println!("Player pos is {}, checking edge {}, inside {}, corner {}", player_pos, board.flat(edge.0, edge.1), board.flat(inside.0, inside.1), idx);
            if player_pos == board.flat(edge.0, edge.1) || !has_any_covered_square(board, qstate, qb_masks, &[board.flat(edge.0, edge.1), board.flat(inside.0, inside.1)], initial_covered_mask) {
                //println!("Flowchart failed because player can reach edge or inside");
                return false;
            }
            if initial_covered_mask & (1u128 << board.flat(edge.0, edge.1)) != 0 || board.flat(edge.0, edge.1) == exit || !board.in_inner(edge.0, edge.1) {
                //println!("Flowchart succeeded");
                return true;
            }
            edge = inward_direction_2.apply(edge.0, edge.1).unwrap();
            inside = inward_direction_2.apply(inside.0, inside.1).unwrap();
        }
    }
    false
}

pub fn run_unreachable_flowcharts(
    board: &QuantumBoard,
    qstate: &QState,
    qb_masks: &[u64],
    player_pos: FlatIdx,
    exit: FlatIdx,
    static_walls: u128,
    initial_covered_mask: u128,
    reachable_stack: &mut Vec<FlatIdx>,
) -> u128 {
    let mut unreachable_inds = 0u128;
    for x in [1, board.width - 2] {
        for y in [1, board.height - 2] {
            let idx = board.flat(x, y);
            let inward_x_direction = if x == 1 { Dir::Right } else { Dir::Left };
            let inward_y_direction = if y == 1 { Dir::Down } else { Dir::Up };
            if corner_unreachable_flowchart(
                board,
                qstate,
                qb_masks,
                player_pos,
                exit,
                static_walls,
                initial_covered_mask,
                reachable_stack,
                idx,
                inward_x_direction,
                inward_y_direction,
            ) || corner_unreachable_flowchart(
                board,
                qstate,
                qb_masks,
                player_pos,
                exit,
                static_walls,
                initial_covered_mask,
                reachable_stack,
                idx,
                inward_y_direction,
                inward_x_direction,
            ) {
                //println!("Proved corner {} is unreachable", idx);
                unreachable_inds |= 1u128 << idx;
            }
        }
    }
    unreachable_inds
}



pub fn is_deadlock(
    board: &QuantumBoard,
    qstate: &QState,
    qb_masks: &[u64],
    player_pos: FlatIdx,
    exit: FlatIdx,
    static_walls: u128,
    initial_covered_mask: u128, // Renamed to avoid shadowing the local variable
    reachable_stack: &mut Vec<FlatIdx>,
    verbose: bool,
) -> bool {
    if player_pos == exit {
        return false;
    }

    let w = board.width as u32;
    let b = initial_covered_mask;

    // 1. Bitwise 2x2 deadlock check for the exit.
    // m_br is a mask of tiles that are the bottom-right corner of a 2x2 covered block.
    let m_br = b & (b >> 1) & (b >> w) & (b >> (w + 1));
    // m_any is a mask of tiles that are part of ANY 2x2 covered block.
    let m_any = m_br | (m_br << 1) | (m_br << w) | (m_br << (w + 1));
    if (m_any & (1u128 << exit)) != 0 {
        return true;
    }

    // 2. Setup Frozen and Inner masks.
    let mut frozen_mask = static_walls;
    let mut inner_mask = 0u128;
    let total_tiles = (board.width * board.height) as usize;
    for i in 0..total_tiles {
        let i_flat = i as FlatIdx;
        if board.in_inner(board.coords(i_flat).0, board.coords(i_flat).1) {
            inner_mask |= 1u128 << i;
        } else {
            frozen_mask |= 1u128 << i;
        }
    }

    // 3. Bitwise Unreachable mask calculation.
    // adj: bits where all 4 cardinal neighbors are covered.
    let adj = (b >> 1) & (b << 1) & (b >> w) & (b << w);
    // diag: bits where at least one opposite diagonal pair is covered.
    let diag = ((b >> (w + 1)) & (b << (w + 1))) | ((b >> (w - 1)) & (b << (w - 1)));

    // A tile is unreachable if neighbors are covered, a diagonal pair is covered,
    // it's not where the player is, and it's within the inner grid.
    let mut unreachable_mask = frozen_mask | (adj & diag & inner_mask & !(1u128 << player_pos));
    //println!("Before: unreachable mask = {:b}", unreachable_mask);
    unreachable_mask |= run_unreachable_flowcharts(board, qstate, qb_masks, player_pos, exit, frozen_mask, initial_covered_mask, reachable_stack);
    //println!("After:  unreachable mask = {:b}", unreachable_mask);

    let mut unfrozen_mask = initial_covered_mask & !frozen_mask;

    loop {
        let mut candidate_new_frozen = unfrozen_mask;
        if candidate_new_frozen == 0 {
            break;
        }

        loop {
            let obstacles = frozen_mask | candidate_new_frozen;
            let mut to_remove = 0u128;

            let mut temp = candidate_new_frozen;
            while temp != 0 {
                let i = temp.trailing_zeros() as u16;
                temp &= !(1 << i);

                let (ix, iy) = board.coords(i);
                let mut can_push = false;
                for dir in Dir::all() {
                    if let (Some(src_c), Some(dst_c)) =
                        (dir.opposite().apply(ix, iy), dir.apply(ix, iy))
                    {
                        let src = board.flat(src_c.0, src_c.1);
                        let dst = board.flat(dst_c.0, dst_c.1);

                        let src_ok = board.in_inner(src_c.0, src_c.1)
                            && (!unreachable_mask & (1 << src)) != 0
                            && src != exit
                            && (obstacles & (1 << src)) == 0;
                        let dst_ok =
                            board.in_inner(dst_c.0, dst_c.1) && (obstacles & (1 << dst)) == 0;
                        if src_ok && dst_ok {
                            can_push = true;
                            break;
                        }
                    }
                }
                if can_push {
                    to_remove |= 1 << i;
                }
            }

            if to_remove == 0 {
                break;
            }
            candidate_new_frozen &= !to_remove;
        }

        /*if verbose {
            println!("Candidate new frozen mask: {:b}", candidate_new_frozen);
            println!("Old frozen mask: {:b}", frozen_mask);
        }*/

        if candidate_new_frozen == 0 {
            break;
        }
        let old_frozen = frozen_mask;
        frozen_mask |= candidate_new_frozen;
        unfrozen_mask &= !candidate_new_frozen;
        if frozen_mask == old_frozen {
            break;
        }
    }

    if (frozen_mask & (1 << exit)) != 0 || (unreachable_mask & (1 << exit)) != 0 {
        return true;
    }
    let final_reachable = get_reachable_mask(
        board,
        player_pos,
        frozen_mask | unreachable_mask,
        reachable_stack,
    );
    if (final_reachable & (1 << exit)) == 0 {
        return true;
    }
    false
}

/// Returns true if any QB's valid current positions are entirely contained within the provided set.
pub fn is_any_qb_fully_covered(
    board: &QuantumBoard,
    qstate: &QState,
    qb_masks: &[u64],
    cover_indices: &[FlatIdx],
) -> bool {
    let mut cover_mask = 0u128;
    for &idx in cover_indices {
        cover_mask |= 1u128 << idx;
    }
    let valid_mask = qstate.valid_mask();
    for &mask in qb_masks {
        let mut qb_valid = mask & valid_mask;
        if qb_valid == 0 {
            continue;
        }
        let mut all_covered = true;
        while qb_valid != 0 {
            let i = qb_valid.trailing_zeros() as usize;
            if (cover_mask & (1u128 << qstate.get(i))) == 0 {
                all_covered = false;
                break;
            }
            qb_valid &= !(1 << i);
        }
        if all_covered {
            return true;
        }
    }
    false
}

//boolean function to check if there is known to be at least one covered square in a set of indices
pub fn has_any_covered_square(
    board: &QuantumBoard,
    qstate: &QState,
    qb_masks: &[u64],
    indices: &[FlatIdx],
    covered_mask: u128,
) -> bool {
    for &idx in indices {
        if (covered_mask & (1u128 << idx)) != 0 {
            return true;
        }
    }
    is_any_qb_fully_covered(board, qstate, qb_masks, indices)
}
