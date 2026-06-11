use crate::board::QuantumBoard;
use crate::types::{Dir, FlatIdx, QBlockId, Tile, PUSH_ALL};
use smallvec::SmallVec;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const INVALID: FlatIdx = u16::MAX;
pub const INVALID_SLOT: u8 = u8::MAX;
pub const MAX_STARTS: usize = 48;
pub const MAX_TILES: usize = 100; // Matches board::MAX_TILES

// ── StartSlot ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct StartSlot {
    pub qb_id: QBlockId,
    pub start_pos: FlatIdx,
    pub is_wall: bool,
    pub push_mask: u8,
}

impl StartSlot {
    #[inline]
    pub fn allows(&self, dir: Dir) -> bool {
        (self.push_mask & dir.mask()) != 0
    }
}

// ── Assignment ────────────────────────────────────────────────────────────────
//
// A concrete assignment: one slot index per QB (compact id), stored as a
// SmallVec. `assign[qb] = slot_index` for each QB.

pub type Assignment = SmallVec<[u8; 8]>;

// ── Shortcut ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub entries: SmallVec<[(FlatIdx, FlatIdx); 16]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutConstraints {
    /// Bitmask of slots that are strictly invalid (cannot be part of the path)
    pub invalid_slots_mask: u64,
    /// Sparse pairs of slots that share a destination and cannot be active together.
    /// Stored compactly as pairs of slot indices: (slot_a, slot_b)
    pub collision_pairs: SmallVec<[(u8, u8); 8]>,
}

impl ShortcutConstraints {
    pub fn from_qstate(s: &QState, slots: &[StartSlot]) -> Self {
        let mut invalid_slots_mask = 0u64;
        let mut collision_pairs = SmallVec::new();

        for i in 0..s.len {
            if s.get(i) == INVALID {
                invalid_slots_mask |= 1 << i;
            } else {
                let pos_i = s.get(i);
                for j in (i + 1)..s.len {
                    // If they share the same physical destination, they collide.
                    if s.get(j) == pos_i && pos_i != INVALID && slots[i].qb_id != slots[j].qb_id {
                        /*println!(
                            "Found collision between slot {} (QB {}) and slot {} (QB {}) at position {}",
                            i, slots[i].qb_id, j, slots[j].qb_id, pos_i
                        );*/
                        collision_pairs.push((i as u8, j as u8));
                    }
                }
            }
        }

        Self {
            invalid_slots_mask,
            collision_pairs,
        }
    }

    /// Returns true ONLY if this shortcut completely covers (is satisfied by) the assignment.
    #[inline(always)]
    pub fn covers_assignment(&self, assignment_mask: u64) -> bool {
        // Unary Rule: If the assignment chooses ANY slot that is invalid in this shortcut, 
        // the assignment is invalid in this shortcut. Thus, the shortcut DOES NOT cover it.
        if (assignment_mask & self.invalid_slots_mask) != 0 {
            return false;
        }

        // Binary Rule: If the assignment chooses BOTH slots of a colliding pair,
        // it causes a collision in this shortcut. A collision makes it invalid in the shortcut.
        // Thus, the shortcut DOES NOT cover it.
        for &(a, b) in &self.collision_pairs {
            let mask_a = 1 << a;
            let mask_b = 1 << b;
            
            // If both bits are set in our assignment mask, we have triggered a collision 
            // in this shortcut's topology, meaning we are NOT covered by it.
            if (assignment_mask & mask_a) != 0 && (assignment_mask & mask_b) != 0 {
                return false;
            }
        }

        // If it passed all invalidation rules, the assignment is completely valid 
        // inside this shortcut. Therefore, the shortcut covers it!
        true
    }
}

// ── QState ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QState {
    /// Slot-centric storage: cur[i] is the FlatIdx of StartSlot i.
    pub cur: [u8; MAX_STARTS],
    /// Number of slots stored in `cur`.
    pub len: usize,
    /// Bitmask of valid slots.
    pub valid_mask: u64,
}

impl std::hash::Hash for QState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.cur[..self.len].hash(state);
    }
}

impl QState {
    pub fn new(slots: &[StartSlot]) -> Self {
        debug_assert!(slots.len() <= MAX_STARTS);
        let mut cur = [INVALID_SLOT; MAX_STARTS];
        let mut valid_mask = 0u64;
        for (i, slot) in slots.iter().enumerate() {
            cur[i] = slot.start_pos as u8;
            valid_mask |= 1 << i;
        }
        Self {
            cur,
            len: slots.len(),
            valid_mask,
        }
    }

    #[inline]
    pub fn get(&self, i: usize) -> FlatIdx {
        let p = self.cur[i];
        if p == INVALID_SLOT {
            INVALID
        } else {
            p as FlatIdx
        }
    }

    #[inline]
    pub fn set(&mut self, i: usize, p: FlatIdx) {
        self.cur[i] = p as u8;
        self.valid_mask |= 1 << i;
    }

    #[inline]
    pub fn invalidate(&mut self, i: usize) {
        self.cur[i] = INVALID_SLOT;
        self.valid_mask &= !(1 << i);
    }

    #[inline]
    pub fn is_valid(&self, i: usize) -> bool {
        self.cur[i] != INVALID_SLOT
    }

    /// Operation 1: Move all slots currently at `from` to `to`.
    #[inline]
    pub fn shift_all(&mut self, from: FlatIdx, to: FlatIdx) {
        let from_u8 = from as u8;
        let to_u8 = to as u8;
        for p in self.cur.iter_mut() {
            if *p == from_u8 {
                *p = to_u8;
            }
        }
    }

    /// Operation 2: Invalidate all slots currently at `pos`.
    #[inline]
    pub fn invalidate_all(&mut self, pos: FlatIdx) {
        let pos_u8 = pos as u8;
        for (i, p) in self.cur.iter_mut().enumerate() {
            if *p == pos_u8 {
                *p = INVALID_SLOT;
                self.valid_mask &= !(1 << i);
            }
        }
    }

    /// Returns a bitmask of all valid slots.
    #[inline]
    pub fn valid_mask(&self) -> u64 {
        self.valid_mask
    }

    /// Convert the index-based QState to a stable StartPos-based Shortcut.
    pub fn to_shortcut(&self, slots: &[StartSlot]) -> Shortcut {
        let mut entries = SmallVec::new();
        for i in 0..self.len {
            let p = self.cur[i];
            if p != INVALID_SLOT {
                entries.push((slots[i].start_pos, p as FlatIdx));
            }
        }
        Shortcut { entries }
    }

    /// Create a QState from a stable Shortcut using the current slot mapping.
    pub fn from_shortcut(s: &Shortcut, slots: &[StartSlot]) -> Self {
        debug_assert!(slots.len() <= MAX_STARTS);
        let mut cur = [INVALID_SLOT; MAX_STARTS];
        let mut valid_mask = 0u64;
        for (i, slot) in slots.iter().enumerate() {
            let value = s
                .entries
                .iter()
                .find(|&&(start, _)| start == slot.start_pos)
                .map_or(INVALID_SLOT, |&(_, curr)| curr as u8);
            if value != INVALID_SLOT {
                valid_mask |= 1 << i;
            }
            cur[i] = value;
        }
        Self {
            cur,
            len: slots.len(),
            valid_mask,
        }
    }
}

// ── Assignment validity and coverage ─────────────────────────────────────────

/// Returns true iff there exists at least one valid assignment of `qstate`.
pub fn has_valid_assignment(qstate: &QState, slots: &[StartSlot], num_qbs: usize, automatically_satisfied_qb_ids: Option<&[u8]>) -> bool {
    find_uncovered_witness(qstate, slots, num_qbs, &[], automatically_satisfied_qb_ids).is_some()
}

/// Returns true iff assignment `a` (slot indices per QB) is a valid assignment
/// of `qstate` S — i.e. every chosen slot is non-INVALID in S and no two
/// chosen slots share a cur value in S.
pub fn assignment_valid_in(a: &Assignment, qstate: &QState) -> bool {
    let mut seen_mask = 0u128;
    for (qb, &slot_i) in a.iter().enumerate() {
        let pos = qstate.get(slot_i as usize);
        if pos == INVALID {
            return false;
        }
        let bit = 1u128 << pos;
        if (seen_mask & bit) != 0 {
            return false;
        }
        seen_mask |= bit;
    }
    true
}

/// Returns true iff assignment `a` is covered by at least one shortcut constraint.
/// An empty assignment (no QBs) is never considered covered: there is nothing
/// to constrain, so shortcuts cannot rule out any configuration.
#[inline]
pub fn assignment_covered(a: &Assignment, shortcuts: &[ShortcutConstraints]) -> bool {
    if a.is_empty() {
        return false;
    }
    shortcuts.iter().any(|s| s.covers_assignment(a.iter().fold(0u64, |mask, &slot_i| mask | (1 << slot_i))))
}

fn qb_order_by_choices(
    choices: &[SmallVec<[u8; 8]>; 16],
    num_qbs: usize,
) -> Option<SmallVec<[u8; 16]>> {
    let mut order: SmallVec<[u8; 16]> = SmallVec::new();
    for qb in 0..num_qbs {
        if choices[qb].is_empty() {
            return None;
        }
        order.push(qb as u8);
    }
    order.sort_unstable_by_key(|&qb| choices[qb as usize].len());
    Some(order)
}

/// Find a valid assignment of `qstate` that is NOT covered by any shortcut constraints.
pub fn find_uncovered_witness(
    qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    automatically_satisfied_qb_ids: Option<&[u8]>,
) -> Option<Assignment> {
    if num_qbs == 0 {
        return if shortcuts.is_empty() { Some(SmallVec::new()) } else { None };
    }

    let mut choices: [SmallVec<[u8; 8]>; 16] = Default::default();
    for (i, slot) in slots.iter().enumerate() {
        if qstate.is_valid(i) {
            choices[slot.qb_id as usize].push(i as u8);
        }
    }

    let qb_order = match qb_order_by_choices(&choices, num_qbs) {
        Some(order) => order,
        None => return None,
    };

    let mut chosen = SmallVec::with_capacity(num_qbs);
    let mut result = None;

    fn backtrack(
        depth: usize,
        qb_order: &[u8],
        chosen: &mut Assignment,
        used_positions: u128,
        chosen_slots_mask: u64,
        choices: &[SmallVec<[u8; 8]>; 16],
        qstate: &QState,
        shortcuts: &[ShortcutConstraints],
        result: &mut Option<Assignment>,
        automatically_satisfied_qb_ids: Option<&[u8]>,
    ) {
        if result.is_some() {
            return;
        }

        if depth == qb_order.len() {
            // Leaf verification: Check if any pre-compiled shortcut covers this assignment mask.
            let mut covered = false;
            for s in shortcuts {
                if s.covers_assignment(chosen_slots_mask) {
                    covered = true;
                    break;
                }
            }
            if !covered {
                *result = Some(chosen.clone());
            }
            return;
        }

        let qb = qb_order[depth] as usize;
        if automatically_satisfied_qb_ids.map_or(false, |ids| ids.contains(&(qb as u8))) {
            // If this QB is automatically satisfied, we can skip it without choosing a slot.
            backtrack(
                depth + 1,
                qb_order,
                chosen,
                used_positions,
                chosen_slots_mask,
                choices,
                qstate,
                shortcuts,
                result,
                automatically_satisfied_qb_ids,
            );
            return;
        }
        for &slot_i in &choices[qb] {
            let idx = slot_i as usize;
            let pos_i = qstate.get(idx);
            let pos_bit = 1u128 << pos_i;

            if (used_positions & pos_bit) != 0 {
                continue;
            }

            chosen.push(slot_i);
            backtrack(
                depth + 1,
                qb_order,
                chosen,
                used_positions | pos_bit,
                chosen_slots_mask | (1 << idx),
                choices,
                qstate,
                shortcuts,
                result,
                automatically_satisfied_qb_ids,
            );
            chosen.pop();
        }
    }

    backtrack(
        0,
        &qb_order,
        &mut chosen,
        0,
        0u64,
        &choices,
        qstate,
        shortcuts,
        &mut result,
        automatically_satisfied_qb_ids,
    );

    result
}

/// Check whether a carried witness assignment is still valid and uncovered
/// after the node's qstate has changed. Returns the witness if still good,
/// or searches for a new one if not.
pub fn refresh_witness(
    // Changed to take witness by reference
    witness: &Assignment,
    qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    automatically_satisfied_qb_ids: Option<&[u8]>,
) -> Option<Assignment> {
    // Fast path: witness still valid and uncovered.
    if assignment_valid_in(witness, qstate) && !assignment_covered(witness, shortcuts) {
        return Some(witness.clone());
    }
    // Search for a new witness.
    find_uncovered_witness(qstate, slots, num_qbs, shortcuts, automatically_satisfied_qb_ids)
}

/// Returns true iff slot `forced` participates in some valid assignment of
/// `qstate` that is not covered by any shortcut.
pub fn slot_has_uncovered_assignment(
    forced: usize,
    qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
) -> bool {
    let forced_qb = slots[forced].qb_id as usize;
    if !qstate.is_valid(forced) {
        return false;
    }

    let mut choices: [SmallVec<[u8; 8]>; 16] = Default::default();
    for (i, slot) in slots.iter().enumerate() {
        if !qstate.is_valid(i) {
            continue;
        }
        let qb = slot.qb_id as usize;
        if qb == forced_qb {
            if i == forced {
                choices[qb].push(i as u8);
            }
        } else {
            choices[qb].push(i as u8);
        }
    }

    let qb_order = match qb_order_by_choices(&choices, num_qbs) {
        Some(order) => order,
        None => return false,
    };

    let mut result = false;
    fn backtrack(
        depth: usize,
        qb_order: &[u8],
        chosen: &mut Assignment,
        used_positions: u128,
        choices: &[SmallVec<[u8; 8]>; 16],
        qstate: &QState,
        shortcuts: &[ShortcutConstraints],
        result: &mut bool,
    ) {
        if *result {
            return;
        }
        if depth == qb_order.len() {
            if !assignment_covered(chosen, shortcuts) {
                *result = true;
            }
            return;
        }
        let qb = qb_order[depth] as usize;
        for &slot_i in &choices[qb] {
            let pos_i = qstate.get(slot_i as usize);
            let pos_bit = 1u128 << pos_i;
            if (used_positions & pos_bit) != 0 {
                continue;
            }
            chosen.push(slot_i);
            backtrack(
                depth + 1,
                qb_order,
                chosen,
                used_positions | pos_bit,
                choices,
                qstate,
                shortcuts,
                result,
            );
            chosen.pop();
            if *result {
                return;
            }
        }
    }

    let mut chosen: Assignment = SmallVec::new();
    backtrack(
        0,
        &qb_order,
        &mut chosen,
        0,
        &choices,
        qstate,
        shortcuts,
        &mut result,
    );
    result
}

/// Operation 6 (Rare): Returns indices of slots at a specific cell.
pub fn slots_at(
    qs: &QState,
    _slots: &[StartSlot],
    cell: FlatIdx,
    _board: &QuantumBoard,
) -> SmallVec<[usize; 8]> {
    let mut res = SmallVec::new();
    let cell_u8 = cell as u8;
    for (i, &p) in qs.cur[..qs.len].iter().enumerate() {
        if p == cell_u8 {
            res.push(i);
        }
    }
    res
}

/// Returns true if any valid slot is currently at the specified cell.
#[inline]
pub fn any_slots_at(qs: &QState, cell: FlatIdx) -> bool {
    let cell_u8 = cell as u8;
    qs.cur[..qs.len].iter().any(|&p| p == cell_u8)
}

/// Returns the raw bitmask of slots at a cell.
pub fn slot_mask_at(qs: &QState, cell: FlatIdx) -> u64 {
    let mut mask = 0u64;
    let cell_u8 = cell as u8;
    for (i, &p) in qs.cur[..qs.len].iter().enumerate() {
        if p == cell_u8 {
            mask |= 1 << i;
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn build_simple_slots() -> (Vec<StartSlot>, QState) {
        let slots = vec![
            StartSlot {
                qb_id: 0,
                start_pos: 1,
                is_wall: false,
                push_mask: PUSH_ALL,
            },
            StartSlot {
                qb_id: 0,
                start_pos: 2,
                is_wall: false,
                push_mask: PUSH_ALL,
            },
            StartSlot {
                qb_id: 1,
                start_pos: 3,
                is_wall: false,
                push_mask: PUSH_ALL,
            },
            StartSlot {
                qb_id: 1,
                start_pos: 4,
                is_wall: false,
                push_mask: PUSH_ALL,
            },
        ];
        let qstate = QState::new(&slots);
        (slots, qstate)
    }

    #[test]
    fn find_uncovered_witness_returns_assignments_not_covered_by_shortcuts() {
        let (slots, qstate) = build_simple_slots();
        let shortcut = Shortcut {
            entries: smallvec![(1, 1), (3, 3)],
        };
        let shortcut_qs = QState::from_shortcut(&shortcut, &slots);
        let shortcut_constraints = ShortcutConstraints::from_qstate(&shortcut_qs, &slots);
        let witness = find_uncovered_witness(&qstate, &slots, 2, &[shortcut_constraints.clone()]);
        assert!(witness.is_some());
        let assignment = witness.unwrap();
        assert_eq!(assignment.len(), 2);
        assert!(!assignment_covered(&assignment, &[shortcut_constraints]));
    }

    #[test]
    fn slot_has_uncovered_assignment_respects_forced_slots() {
        let (slots, qstate) = build_simple_slots();
        let shortcut = Shortcut {
            entries: smallvec![(1, 1), (3, 3)],
        };
        let shortcut_qs = QState::from_shortcut(&shortcut, &slots);
        let shortcut_constraints = ShortcutConstraints::from_qstate(&shortcut_qs, &slots);
        assert!(slot_has_uncovered_assignment(
            1,
            &qstate,
            &slots,
            2,
            &[shortcut_constraints.clone()]
        ));
        assert!(slot_has_uncovered_assignment(
            2,
            &qstate,
            &slots,
            2,
            &[shortcut_constraints]
        ));
    }
}
