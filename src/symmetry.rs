use crate::types::Dir;

/// Direction permutations of board isometries fixing the exit.
pub(crate) fn exit_symmetries(width: u16, height: u16, x: u16, y: u16) -> Vec<[Dir; 4]> {
    use Dir::*;
    let candidates = [
        ([Up, Down, Right, Left], width + 1 - x, y, true),
        ([Down, Up, Left, Right], x, height + 1 - y, true),
        ([Down, Up, Right, Left], width + 1 - x, height + 1 - y, true),
        ([Left, Right, Up, Down], y, x, width == height),
        (
            [Right, Left, Down, Up],
            (width + 1).saturating_sub(y),
            (height + 1).saturating_sub(x),
            width == height,
        ),
        (
            [Right, Left, Up, Down],
            (width + 1).saturating_sub(y),
            x,
            width == height,
        ),
        (
            [Left, Right, Down, Up],
            y,
            (height + 1).saturating_sub(x),
            width == height,
        ),
    ];
    candidates
        .into_iter()
        .filter_map(|(map, tx, ty, valid)| (valid && tx == x && ty == y).then_some(map))
        .collect()
}

pub(crate) fn canonical_path(path: &[Dir], symmetries: &[[Dir; 4]]) -> bool {
    symmetries.iter().all(|map| {
        // Stored paths are forward solutions; compare chronological undo moves.
        for dir in path.iter().rev().map(|dir| dir.opposite()) {
            match dir.idx().cmp(&map[dir.idx()].idx()) {
                std::cmp::Ordering::Less => return true,
                std::cmp::Ordering::Greater => return false,
                std::cmp::Ordering::Equal => {}
            }
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_stabilizers_and_prefix_orbits() {
        assert_eq!(exit_symmetries(5, 7, 3, 3).len(), 1);
        assert_eq!(exit_symmetries(5, 7, 3, 4).len(), 3);
        assert_eq!(exit_symmetries(5, 5, 3, 3).len(), 7);
        assert_eq!(exit_symmetries(4, 4, 2, 2).len(), 1);
        assert!(exit_symmetries(4, 5, 2, 2).is_empty());
        let maps = exit_symmetries(5, 5, 3, 3);
        for encoded in 0..4096usize {
            let undo: Vec<_> = (0..6)
                .map(|i| Dir::all()[(encoded >> (2 * i)) & 3])
                .collect();
            let mut orbit = vec![undo.clone()];
            orbit.extend(
                maps.iter()
                    .map(|map| undo.iter().map(|d| map[d.idx()]).collect()),
            );
            orbit.sort_by_key(|p| p.iter().map(|d| d.idx()).collect::<Vec<_>>());
            orbit.dedup();
            let survivors: Vec<_> = orbit
                .iter()
                .filter(|p| {
                    let path: Vec<_> = p.iter().rev().map(|d| d.opposite()).collect();
                    canonical_path(&path, &maps)
                })
                .collect();
            assert_eq!(survivors, vec![&orbit[0]]);
            for p in orbit {
                let path: Vec<_> = p.iter().rev().map(|d| d.opposite()).collect();
                if !canonical_path(&path, &maps) {
                    for d in Dir::all() {
                        let mut child = vec![d.opposite()];
                        child.extend_from_slice(&path);
                        assert!(!canonical_path(&child, &maps));
                    }
                }
            }
        }
    }
}
