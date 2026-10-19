//! Bounded hints, never proofs: every inherited path is replayed on the new board.
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::board::MAX_TILES;
use crate::bfs::StateKey;
use crate::types::{Dir, FlatIdx};

const MAX_PATHS: usize = 8;
const MAX_PATH_LENGTH: usize = 1024;
const MAX_TRACE_NODES: usize = 4096;

#[derive(Clone, Default)]
pub struct ShortcutPaths {
    paths: Vec<Vec<Dir>>,
}

impl ShortcutPaths {
    pub fn paths(&self) -> &[Vec<Dir>] {
        &self.paths
    }

    pub fn remember(&mut self, path: Vec<Dir>) {
        if path.is_empty() || path.len() > MAX_PATH_LENGTH || self.paths.contains(&path) {
            return;
        }
        // Prefer the shortest hints, with stable insertion order for ties.
        let at = self.paths.partition_point(|p| p.len() <= path.len());
        if at < MAX_PATHS {
            self.paths.insert(at, path);
            self.paths.truncate(MAX_PATHS);
        }
    }

    pub fn prepend(&mut self, dir: Dir) {
        for path in &mut self.paths {
            path.insert(0, dir);
        }
        self.paths.retain(|p| p.len() <= MAX_PATH_LENGTH);
    }
}

// Immutable arena edges ensure a cheaper queued arrival cannot rewrite the
// ancestry of an already recorded descendant. The map points to the best trace.
pub(crate) struct PathTracker {
    indices: FxHashMap<StateKey, usize>,
    arena: Vec<Trace>,
}

struct Trace {
    parent: Option<usize>,
    segment: SmallVec<[Dir; 8]>,
}

impl PathTracker {
    pub(crate) fn new(root: StateKey) -> Self {
        Self {
            indices: FxHashMap::from_iter([(root, 0)]),
            arena: vec![Trace {
                parent: None,
                segment: SmallVec::new(),
            }],
        }
    }

    pub(crate) fn record(
        &mut self,
        child: StateKey,
        parent: &StateKey,
        mut walk_end: FlatIdx,
        final_move: Option<Dir>,
        dists: &[u8; MAX_TILES],
        width: u16,
    ) {
        if self.arena.len() >= MAX_TRACE_NODES {
            self.indices.remove(&child);
            return;
        }
        let Some(&parent) = self.indices.get(parent) else {
            self.indices.remove(&child);
            return;
        };
        let mut segment = SmallVec::new();
        while dists[walk_end as usize] != 0 {
            let distance = dists[walk_end as usize];
            let previous = Dir::all().into_iter().find_map(|dir| {
                let (x, y) = dir.opposite().apply(walk_end % width, walk_end / width)?;
                let pos = y as usize * width as usize + x as usize;
                (x < width
                    && pos < MAX_TILES
                    && dists[pos] < distance
                    && dists[pos] + 1 == distance)
                    .then_some((pos as FlatIdx, dir))
            });
            let Some((pos, dir)) = previous else {
                self.indices.remove(&child);
                return;
            };
            segment.push(dir);
            walk_end = pos;
        }
        segment.reverse();
        if let Some(dir) = final_move {
            segment.push(dir);
        }
        let index = self.arena.len();
        self.arena.push(Trace {
            parent: Some(parent),
            segment,
        });
        self.indices.insert(child, index);
    }

    pub(crate) fn path(&self, key: &StateKey) -> Option<Vec<Dir>> {
        let mut current = *self.indices.get(key)?;
        let mut path = Vec::new();
        while let Some(parent) = self.arena[current].parent {
            path.extend(self.arena[current].segment.iter().rev());
            if path.len() > MAX_PATH_LENGTH {
                return None;
            }
            current = parent;
        }
        path.reverse();
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_reconstructs_weighted_walk_and_push_without_rewriting_ancestors() {
        let key = |player| crate::bfs::state_key(player, &crate::qstate::QState::new(&[]), &crate::deadlock::BoxState::default());
        let mut tracker = PathTracker::new(key(1));
        let mut dists = [u8::MAX; MAX_TILES];
        dists[7] = 0;
        dists[8] = 1;
        dists[9] = 2;
        tracker.record(key(2), &key(1), 9, Some(Dir::Down), &dists, 6);
        assert_eq!(
            tracker.path(&key(2)),
            Some(vec![Dir::Right, Dir::Right, Dir::Down])
        );
        tracker.record(key(3), &key(2), 8, None, &dists, 6);
        tracker.record(key(2), &key(1), 7, Some(Dir::Down), &dists, 6);
        assert_eq!(tracker.path(&key(2)), Some(vec![Dir::Down]));
        assert_eq!(
            tracker.path(&key(3)),
            Some(vec![Dir::Right, Dir::Right, Dir::Down, Dir::Right])
        );
    }

    #[test]
    fn hints_are_bounded_unique_and_prefixed() {
        let mut paths = ShortcutPaths::default();
        for n in 1..20 {
            paths.remember(vec![Dir::Left; n]);
        }
        paths.remember(vec![Dir::Left]);
        assert_eq!(paths.paths.len(), MAX_PATHS);
        paths.prepend(Dir::Up);
        assert_eq!(paths.paths[0], vec![Dir::Up, Dir::Left]);
    }
}
