use crate::board::QuantumBoard;
use crate::types::{Dir, FlatIdx, QBlockId, Tile, PUSH_ALL};
use smallvec::SmallVec;
use rustc_hash::FxHashMap;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const INVALID: FlatIdx = u16::MAX;
pub const INVALID_SLOT: u8 = u8::MAX;
pub const FREE_SLOT: u8 = 127;
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
// A concrete assignment: one selected slot per non-deleted QB, in the
// assignment search's choice order (not necessarily QB order).

pub type Assignment = SmallVec<[u8; 8]>;

/// Witness IDs are local to a solve. A slot bitset is an exact assignment key,
/// independent of the order in which the assignment search chose its QBs.
#[derive(Default)]
pub struct AssignmentPool {
    assignments: Vec<Assignment>,
    indices: FxHashMap<u64, usize>,
}

impl AssignmentPool {
    pub fn intern(&mut self, assignment: Assignment) -> usize {
        let mask = assignment.iter().fold(0u64, |mask, &slot| mask | (1u64 << slot));
        *self.indices.entry(mask).or_insert_with(|| {
            let index = self.assignments.len();
            self.assignments.push(assignment);
            index
        })
    }
}

impl std::ops::Deref for AssignmentPool {
    type Target = [Assignment];

    fn deref(&self) -> &Self::Target {
        &self.assignments
    }
}

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
        if p == INVALID {
            self.invalidate(i);
            return;
        }
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
pub fn has_valid_assignment(qstate: &QState, slots: &[StartSlot], num_qbs: usize) -> bool {
    find_uncovered_witness(qstate, slots, num_qbs, &[]).is_some()
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
        if pos == FREE_SLOT as FlatIdx {
            continue;
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
    let mask = a.iter().fold(0u64, |mask, &slot_i| mask | (1 << slot_i));
    shortcuts.iter().any(|s| s.covers_assignment(mask))
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

/// Unique positions worth restoring for each invalid slot during dominance checks.
pub(crate) fn imputation_candidates(qstate: &QState, slots: &[StartSlot]) -> SmallVec<[(usize, FlatIdx); 16]> {
    let mut positions = [0u128; 16];
    for (i, slot) in slots.iter().enumerate() {
        if qstate.is_valid(i) {
            positions[slot.qb_id as usize] |= 1u128 << qstate.get(i);
        }
    }
    let mut possible = SmallVec::new();
    for (i, slot) in slots.iter().enumerate() {
        if qstate.is_valid(i) { continue; }
        let mut candidates = positions[slot.qb_id as usize] | (1u128 << slot.start_pos);
        while candidates != 0 {
            let pos = candidates.trailing_zeros() as FlatIdx;
            candidates &= candidates - 1;
            possible.push((i, pos));
        }
    }
    possible
}

/// A sufficient coverage proof for the whole domain, without choosing slots.
fn domain_covered(qstate: &QState, available: u64, shortcuts: &[ShortcutConstraints]) -> bool {
    shortcuts.iter().any(|shortcut| {
        if shortcut.invalid_slots_mask & available != 0 {
            return false;
        }
        // A shortcut's collision can only exclude an assignment if both slots
        // can coexist in this state. Otherwise every valid choice is covered.
        shortcut.collision_pairs.iter().all(|&(a, b)| {
            let pair = (1u64 << a) | (1u64 << b);
            available & pair != pair
                || (qstate.cur[a as usize] == qstate.cur[b as usize]
                    && qstate.cur[a as usize] != FREE_SLOT)
        })
    })
}

/// Find a valid assignment of `qstate` that is NOT covered by any shortcut constraints.
pub fn find_uncovered_witness(
    qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
) -> Option<Assignment> {
    find_witness_with_forced_slot(qstate, slots, num_qbs, shortcuts, None)
}

fn find_witness_with_forced_slot(
    qstate: &QState,
    slots: &[StartSlot],
    num_qbs: usize,
    shortcuts: &[ShortcutConstraints],
    forced: Option<usize>,
) -> Option<Assignment> {
    #[cfg(feature = "profile-search")]
    let _timer = crate::search_profile::Timer::start(2);
    if num_qbs == 0 {
        return if shortcuts.is_empty() { Some(SmallVec::new()) } else { None };
    }
    let mut choices: [SmallVec<[u8; 8]>; 16] = Default::default();
    let mut available = 0u64;
    for (i, slot) in slots.iter().enumerate() {
        if qstate.is_valid(i)
            && forced.map_or(true, |f| slot.qb_id != slots[f].qb_id || i == f)
        {
            choices[slot.qb_id as usize].push(i as u8);
            if qstate.cur[i] != FREE_SLOT { available |= 1u64 << i; }
        }
    }
    if domain_covered(qstate, available, shortcuts) { return None; }

    let qb_order = match qb_order_by_choices(&choices, num_qbs) {
        Some(order) => order,
        None => return None,
    };

    let mut chosen = SmallVec::with_capacity(num_qbs);
    let mut result = None;
    // After choosing a QB, its other slots cannot appear in any completion.
    // A shortcut covering this smaller domain rules out the entire branch.
    let mut suffix = [0u64; 17];
    for depth in (0..qb_order.len()).rev() {
        suffix[depth] = suffix[depth + 1];
        for &i in &choices[qb_order[depth] as usize] {
            if qstate.cur[i as usize] != FREE_SLOT { suffix[depth] |= 1u64 << i; }
        }
    }

    fn backtrack(
        depth: usize,
        qb_order: &[u8],
        chosen: &mut Assignment,
        used_positions: u128,
        chosen_slots_mask: u64,
        choices: &[SmallVec<[u8; 8]>; 16],
        suffix: &[u64; 17],
        qstate: &QState,
        shortcuts: &[ShortcutConstraints],
        result: &mut Option<Assignment>,
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

        if depth > 0 && domain_covered(qstate, chosen_slots_mask | suffix[depth], shortcuts) {
            return;
        }

        let qb = qb_order[depth] as usize;
        if qstate.cur[choices[qb][0] as usize] == FREE_SLOT {
            // If this QB is automatically satisfied, we can skip it without choosing a slot.
            backtrack(
                depth + 1,
                qb_order,
                chosen,
                used_positions,
                chosen_slots_mask,
                choices,
                suffix,
                qstate,
                shortcuts,
                result,
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
                suffix,
                qstate,
                shortcuts,
                result,
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
        &suffix,
        qstate,
        shortcuts,
        &mut result,
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
) -> Option<Assignment> {
    // Fast path: witness still valid and uncovered.
    if assignment_valid_in(witness, qstate) && !assignment_covered(witness, shortcuts) {
        return Some(witness.clone());
    }
    // Search for a new witness.
    find_uncovered_witness(qstate, slots, num_qbs, shortcuts)
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
    if !qstate.is_valid(forced) {
        return false;
    }
    find_witness_with_forced_slot(qstate, slots, num_qbs, shortcuts, Some(forced)).is_some()
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

    #[test]
    fn witness_pool_keys_are_exact_and_ignore_choice_order() {
        let mut pool = AssignmentPool::default();
        let a = pool.intern(smallvec![0, 8, 47]);
        assert_eq!(a, pool.intern(smallvec![47, 0, 8]));
        let b = pool.intern(smallvec![0, 9, 47]);
        assert_ne!(a, b);
        assert_eq!(pool[a].as_slice(), &[0, 8, 47]);
        assert_eq!(pool[b].as_slice(), &[0, 9, 47]);
        let empty = pool.intern(smallvec![]);
        assert_eq!(empty, pool.intern(smallvec![]));
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn setting_invalid_updates_both_representations() {
        let (slots, mut state) = build_simple_slots();
        state.set(0, INVALID);
        assert!(!state.is_valid(0));
        assert_eq!(state.valid_mask() & 1, 0);
        state.set(0, slots[0].start_pos);
        assert!(state.is_valid(0));
        assert_eq!(state.valid_mask() & 1, 1);
    }

    #[test]
    fn domain_coverage_matches_exhaustive_assignments() {
        let (slots, _) = build_simple_slots();
        // Includes cross-QB collisions, invalid slots, and deleted QBs.
        let positions = [0, 1, 2, INVALID, FREE_SLOT as FlatIdx];
        for state_code in 0..625 {
            let mut state = QState::new(&slots);
            let mut code = state_code;
            for i in 0..4 {
                state.set(i, positions[code % 5]);
                code /= 5;
            }
            // Deleted QBs have every slot free, never mixed with physical slots.
            if (0..2).any(|qb| (state.get(qb * 2) == FREE_SLOT as FlatIdx)
                != (state.get(qb * 2 + 1) == FREE_SLOT as FlatIdx)) { continue; }
            for shortcut_code in 0..256 {
                let mut shortcut = QState::new(&slots);
                let mut code = shortcut_code;
                for i in 0..4 {
                    shortcut.set(i, positions[code % 4]);
                    code /= 4;
                }
                let constraints = [ShortcutConstraints::from_qstate(&shortcut, &slots)];
                let mut expected = false;
                for a in 0..2 {
                    for b in 2..4 {
                        if !state.is_valid(a) || !state.is_valid(b) { continue; }
                        let mut assignment = Assignment::new();
                        for i in [a, b] {
                            if state.get(i) != FREE_SLOT as FlatIdx { assignment.push(i as u8); }
                        }
                        let mask = assignment.iter().fold(0, |mask, &i| mask | (1u64 << i));
                        expected |= assignment_valid_in(&assignment, &state)
                            && !constraints[0].covers_assignment(mask);
                    }
                }
                let actual = find_uncovered_witness(&state, &slots, 2, &constraints);
                assert_eq!(actual.is_some(), expected, "state={state_code} shortcut={shortcut_code}");
            }
        }
    }

    #[test]
    fn deleted_qbs_do_not_collide_at_the_sentinel() {
        let (_, mut state) = build_simple_slots();
        state.set(0, FREE_SLOT as FlatIdx);
        state.set(2, FREE_SLOT as FlatIdx);
        assert!(assignment_valid_in(&smallvec![0, 2], &state));
    }

    #[test]
    fn partial_coverage_and_forced_choices_match_exhaustive_three_qb_assignments() {
        let slots: Vec<_> = (0..6).map(|i| StartSlot {
            qb_id: i / 2, start_pos: i as FlatIdx, is_wall: false, push_mask: PUSH_ALL,
        }).collect();
        let mut seed = 17u64;
        let mut random = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as usize
        };
        for case in 0..2048 {
            let mut state = QState::new(&slots);
            for i in 0..6 {
                state.set(i, [1, 2, 3, 4, INVALID][random() % 5]);
            }
            if case % 5 == 0 {
                let qb = random() % 3;
                state.set(qb * 2, FREE_SLOT as FlatIdx);
                state.set(qb * 2 + 1, FREE_SLOT as FlatIdx);
            }
            let constraints: Vec<_> = (0..case % 5).map(|_| {
                let mut shortcut = QState::new(&slots);
                for i in 0..6 { shortcut.set(i, [1, 2, 3, INVALID][random() % 4]); }
                ShortcutConstraints::from_qstate(&shortcut, &slots)
            }).collect();
            let mut expected = false;
            let mut forced_expected = [false; 6];
            for code in 0..8 {
                let choices = [code & 1, 2 + ((code >> 1) & 1), 4 + ((code >> 2) & 1)];
                if choices.iter().any(|&i| !state.is_valid(i)) { continue; }
                let assignment: Assignment = choices.iter().copied()
                    .filter(|&i| state.cur[i] != FREE_SLOT).map(|i| i as u8).collect();
                let mask = assignment.iter().fold(0u64, |m, &i| m | (1 << i));
                if assignment_valid_in(&assignment, &state)
                    && !constraints.iter().any(|c| c.covers_assignment(mask)) {
                    expected = true;
                    for i in choices { forced_expected[i] = true; }
                }
            }
            let witness = find_uncovered_witness(&state, &slots, 3, &constraints);
            assert_eq!(witness.is_some(), expected, "case={case}");
            if let Some(witness) = witness {
                assert!(assignment_valid_in(&witness, &state));
                assert!(!assignment_covered(&witness, &constraints));
            }
            for (i, expected) in forced_expected.into_iter().enumerate() {
                assert_eq!(slot_has_uncovered_assignment(i, &state, &slots, 3, &constraints), expected,
                    "case={case} forced={i}");
            }
        }
    }

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
