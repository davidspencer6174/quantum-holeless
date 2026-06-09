// board.rs — QuantumBoard: storage, access, and mutation.

use crate::types::{
    fis_contains, fis_from_iter, fis_single, FlatIdx, FlatIdxSet, QBlockId, QuantumBlock,
    PUSH_NONE, Tile,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

pub const MAX_TILES: usize = 100;
pub const MAX_BLOCKS: usize = 16;

/// A Sokoban-variant board that may contain QuantumBlocks.
///
/// Padded with a one-cell wall border on all sides.
/// `width` and `height` include the padding; the inner playfield
/// occupies `(1,1)` .. `(width-2, height-2)` inclusive.
///
/// Every box on the board — whether originally quantum or concrete — is
/// represented as a QuantumBlock. Tile::Box is still used for static input
/// boxes that will never be pushed (currently kept for io/backward compat).
/// Tile::Wall is used for permanent walls.
#[derive(Clone, Debug)]
pub struct QuantumBoard {
    pub width: u16,
    pub height: u16,
    /// Row-major tile array, fixed size. Only [0..width*height] is used.
    pub tiles: [Tile; MAX_TILES],
    /// Pushability mask for concrete boxes at a specific tile index.
    pub box_push_masks: [crate::types::PushMask; MAX_TILES],
    /// All QuantumBlocks on this board, indexed by QBlockId.
    /// Slots are never removed; a QB with no valid positions means the block
    /// has been fully resolved and the slot is logically inactive.
    pub quantum_blocks: SmallVec<[QuantumBlock; MAX_BLOCKS]>,
}

impl QuantumBoard {
    // ── Construction ──────────────────────────────────────────────────────

    pub fn new(inner_width: u16, inner_height: u16) -> Self {
        let width = inner_width + 2;
        let height = inner_height + 2;
        debug_assert!(
            (width * height) as usize <= MAX_TILES,
            "board size {}x{} exceeds MAX_TILES={}",
            width,
            height,
            MAX_TILES
        );
        Self {
            width,
            height,
            tiles: [Tile::Wall; MAX_TILES],
            box_push_masks: [crate::types::PUSH_NONE; MAX_TILES],
            quantum_blocks: SmallVec::new(),
        }
    }

    pub fn wall_filled_with_player(
        inner_width: u16,
        inner_height: u16,
        goal_x: u16,
        goal_y: u16,
    ) -> Self {
        let mut b = Self::new(inner_width, inner_height);
        b.set(goal_x, goal_y, Tile::Player);
        b
    }

    #[inline]
    pub fn get_box_push_mask(&self, pos: FlatIdx) -> crate::types::PushMask {
        self.box_push_masks[pos as usize]
    }

    #[inline]
    pub fn set_box_push_mask(&mut self, pos: FlatIdx, mask: crate::types::PushMask) {
        self.box_push_masks[pos as usize] = mask;
    }

    // ── Coordinate helpers ────────────────────────────────────────────────

    #[inline]
    pub fn flat(&self, x: u16, y: u16) -> FlatIdx {
        y * self.width + x
    }
    #[inline]
    pub fn coords(&self, idx: FlatIdx) -> (u16, u16) {
        (idx % self.width, idx / self.width)
    }
    #[inline]
    pub fn in_inner(&self, x: u16, y: u16) -> bool {
        x >= 1 && y >= 1 && x < self.width - 1 && y < self.height - 1
    }

    // ── Tile access ───────────────────────────────────────────────────────

    #[inline]
    pub fn get(&self, x: u16, y: u16) -> Tile {
        self.tiles[self.flat(x, y) as usize]
    }
    #[inline]
    pub fn get_flat(&self, idx: FlatIdx) -> Tile {
        self.tiles[idx as usize]
    }
    #[inline]
    pub fn set(&mut self, x: u16, y: u16, tile: Tile) {
        self.tiles[self.flat(x, y) as usize] = tile;
    }
    #[inline]
    pub fn set_flat(&mut self, idx: FlatIdx, tile: Tile) {
        self.tiles[idx as usize] = tile;
    }

    // ── QuantumBlock access ───────────────────────────────────────────────

    pub fn quantum_at(&self, x: u16, y: u16) -> Option<(QBlockId, &QuantumBlock)> {
        match self.get(x, y) {
            Tile::Quantum(id) => Some((id, &self.quantum_blocks[id as usize])),
            _ => None,
        }
    }

    pub fn quantum_at_flat(&self, idx: FlatIdx) -> Option<(QBlockId, &QuantumBlock)> {
        match self.get_flat(idx) {
            Tile::Quantum(id) => Some((id, &self.quantum_blocks[id as usize])),
            _ => None,
        }
    }

    // ── QuantumBlock mutation ─────────────────────────────────────────────

    /// Add a new QuantumBlock. `cells` maps current_pos → start_positions.
    /// Marks all current positions as Quantum(id) in the tile array.
    pub fn add_quantum_block_with_starts(
        &mut self,
        cells: FxHashMap<FlatIdx, FlatIdxSet>,
        wall_pos: Option<FlatIdx>,
    ) -> QBlockId {
        let id = self.quantum_blocks.len() as QBlockId;
        for &flat in cells.keys() {
            self.set_flat(flat, Tile::Quantum(id));
        }
        self.quantum_blocks.push(QuantumBlock::new(cells, wall_pos));
        id
    }

    /// Add a new QuantumBlock where each position is its own start.
    /// Used when parsing: all cells are potentially valid positions.
    pub fn add_quantum_block(
        &mut self,
        positions: impl IntoIterator<Item = FlatIdx>,
        wall_pos: Option<FlatIdx>,
    ) -> QBlockId {
        let id = self.quantum_blocks.len() as QBlockId;
        let mut cells: FxHashMap<FlatIdx, FlatIdxSet> = FxHashMap::default();
        for flat in positions {
            self.set_flat(flat, Tile::Quantum(id));
            cells.insert(flat, fis_single(flat));
        }
        self.quantum_blocks.push(QuantumBlock::new(cells, wall_pos));
        id
    }

    /// Remove a current position from a QB. If this empties the QB,
    /// returns true (caller should handle the "no valid positions" case).
    pub fn qb_remove_current(&mut self, id: QBlockId, pos: FlatIdx) -> bool {
        self.quantum_blocks[id as usize].remove_current(pos);
        if self.get_flat(pos) == Tile::Quantum(id) {
            self.set_flat(pos, Tile::Floor);
        }
        // If the removed position was the wall_pos, clear it: the wall
        // interpretation is no longer in this QB's domain.
        if self.quantum_blocks[id as usize].wall_pos == Some(pos) {
            self.quantum_blocks[id as usize].wall_pos = None;
        }
        self.quantum_blocks[id as usize].is_empty()
    }

    /// Restrict starts at a current position to the given set (intersect).
    pub fn qb_restrict_starts(&mut self, id: QBlockId, pos: FlatIdx, allowed: &FlatIdxSet) {
        self.quantum_blocks[id as usize].restrict_starts_at(pos, allowed);
    }

    /// Resolve a QB: place its box at `box_pos`, clear all other current positions.
    /// The QB slot remains but becomes a concrete-box QB: {box_pos: {box_pos}}.
    /// wall_pos cells that aren't box_pos become Floor.
    pub fn resolve_qb_to_box(&mut self, id: QBlockId, box_pos: FlatIdx, box_starts: FlatIdxSet) {
        let id8 = id as QBlockId;
        let positions: Vec<FlatIdx> = self.quantum_blocks[id as usize]
            .current_positions()
            .collect();
        for pos in positions {
            if pos != box_pos && self.get_flat(pos) == Tile::Quantum(id8) {
                self.set_flat(pos, Tile::Floor);
            }
        }
        // Mark the destination tile as Quantum(id) so the board correctly reflects
        // the box's new position (it may have been Floor, Player, etc. before).
        self.set_flat(box_pos, Tile::Quantum(id8));
        let mut new_push_masks: FxHashMap<FlatIdx, crate::types::PushMask> = FxHashMap::default();
        for &start in box_starts.iter() {
            let mask = self.quantum_blocks[id as usize].push_mask_for_start(start);
            new_push_masks.insert(start, mask);
        }
        let mut new_cells: FxHashMap<FlatIdx, FlatIdxSet> = FxHashMap::default();
        new_cells.insert(box_pos, box_starts);
        self.quantum_blocks[id as usize] = QuantumBlock::new_with_push_masks(new_cells, None, new_push_masks);
    }

    // ── Miscellaneous ─────────────────────────────────────────────────────

    pub fn find_player(&self) -> FlatIdx {
        self.tiles[..(self.width * self.height) as usize]
            .iter()
            .position(|t| *t == Tile::Player)
            .expect("no Player tile on board") as FlatIdx
    }

    /// Replace every single-tile QB with either a concrete Wall or a concrete
    /// Box, depending on whether its sole current position is the wall_pos.
    /// This eliminates QB overhead in boards where all uncertainty has resolved.
    pub fn collapse_single_tile_qbs(&mut self) {
        for id in 0..self.quantum_blocks.len() {
            let qb = &self.quantum_blocks[id];
            if qb.num_positions() != 1 {
                continue;
            }
            let pos = qb.current_positions().next().unwrap();
            let is_wall = qb.wall_pos == Some(pos);
            let mask = if !is_wall {
                qb.box_push_mask_at(pos)
            } else {
                PUSH_NONE
            };
            let tile = if is_wall { Tile::Wall } else { Tile::Box };
            self.set_flat(pos, tile);
            if !is_wall {
                self.set_box_push_mask(pos, mask);
            } else {
                self.set_box_push_mask(pos, PUSH_NONE);
            }
            self.quantum_blocks[id].cells.clear();
            self.quantum_blocks[id].wall_pos = None;
            self.quantum_blocks[id].push_masks.clear();
        }
    }

    /// Remove any empty QuantumBlocks and re-index the Tile::Quantum IDs on the grid.
    pub fn compact_blocks(&mut self) {
        if self.quantum_blocks.iter().all(|qb| !qb.is_empty()) {
            return;
        }

        let mut old_to_new = [0u8; MAX_BLOCKS];
        let mut next_id = 0u8;
        let mut new_blocks = SmallVec::new();

        for (old_id, qb) in self.quantum_blocks.iter().enumerate() {
            if !qb.is_empty() {
                old_to_new[old_id] = next_id;
                new_blocks.push(qb.clone());
                next_id += 1;
            }
        }

        self.quantum_blocks = new_blocks;

        let total = (self.width * self.height) as usize;
        for tile in self.tiles[..total].iter_mut() {
            if let Tile::Quantum(id) = *tile {
                *tile = Tile::Quantum(old_to_new[id as usize]);
            }
        }
    }
}

// ── Display ───────────────────────────────────────────────────────────────────

impl std::fmt::Display for QuantumBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for y in 0..self.height {
            for x in 0..self.width {
                let ch = match self.get(x, y) {
                    Tile::Wall => '#',
                    Tile::Floor => '.',
                    Tile::Box => '$',
                    Tile::Player => '@',
                    Tile::Quantum(_) => '?',
                };
                write!(f, "{ch}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn board5() -> QuantumBoard {
        QuantumBoard::new(3, 3)
    }

    #[test]
    fn coord_roundtrip() {
        let b = board5();
        for y in 0..b.height {
            for x in 0..b.width {
                assert_eq!(b.coords(b.flat(x, y)), (x, y));
            }
        }
    }

    #[test]
    fn add_row_block() {
        let mut b = board5();
        let positions = [b.flat(1, 2), b.flat(2, 2), b.flat(3, 2)];
        let end = b.flat(3, 2);
        let id = b.add_quantum_block(positions, Some(end));
        for x in 1..=3u16 {
            assert_eq!(b.get(x, 2), Tile::Quantum(id));
        }
        assert_eq!(b.quantum_blocks[id as usize].num_positions(), 3);
    }

    #[test]
    fn add_col_block() {
        let mut b = board5();
        let positions = [b.flat(2, 1), b.flat(2, 2), b.flat(2, 3)];
        let id = b.add_quantum_block(positions, None);
        for y in 1..=3u16 {
            assert_eq!(b.get(2, y), Tile::Quantum(id));
        }
        assert_eq!(b.quantum_blocks[id as usize].num_positions(), 3);
    }

    #[test]
    fn remove_current() {
        let mut b = board5();
        let s = b.flat(1, 2);
        let m = b.flat(2, 2);
        let e = b.flat(3, 2);
        let id = b.add_quantum_block([s, m, e], Some(s));
        b.qb_remove_current(id, s);
        assert_eq!(b.get(1, 2), Tile::Floor);
        assert_eq!(b.quantum_blocks[id as usize].num_positions(), 2);
    }

    #[test]
    fn resolve_to_box() {
        let mut b = board5();
        let s = b.flat(1, 2);
        let m = b.flat(2, 2);
        let e = b.flat(3, 2);
        let id = b.add_quantum_block([s, m, e], Some(s));
        let box_starts = b.quantum_blocks[id as usize].box_starts_at(m);
        b.resolve_qb_to_box(id, m, box_starts);
        assert_eq!(b.get(1, 2), Tile::Floor);
        assert_eq!(b.get(2, 2), Tile::Quantum(id));
        assert_eq!(b.get(3, 2), Tile::Floor);
        assert!(b.quantum_blocks[id as usize].is_concrete_box());
    }
}
