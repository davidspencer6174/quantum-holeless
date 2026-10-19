use rustc_hash::{FxHashMap, FxHashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(crate) struct Checkpoint {
    _lock: File,
    journal: File,
    pub(crate) directory: PathBuf,
    completed: FxHashSet<Vec<u8>>,
    pub(crate) best: usize,
    artifacts: FxHashMap<String, Vec<u8>>,
    dirty_artifacts: FxHashSet<String>,
    record_contents: Vec<u8>,
    record_name: String,
    record_dirty: bool,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

fn lock(directory: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(directory.join("run.lock"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(file)
}

impl Checkpoint {
    pub(crate) fn open(
        directory: &Path,
        metadata: &str,
        resume: bool,
        record_name: &str,
    ) -> io::Result<Self> {
        if resume {
            if !directory.is_dir() {
                return Err(invalid("checkpoint directory does not exist"));
            }
        } else {
            std::fs::create_dir_all(directory)?;
        }
        let directory = std::fs::canonicalize(directory)?;
        let guard = lock(&directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        if !resume {
            options.create_new(true);
        }
        let journal = options.open(directory.join("checkpoint.bin"))?;
        let mut checkpoint = Self {
            _lock: guard,
            journal,
            directory,
            completed: FxHashSet::default(),
            best: 0,
            artifacts: FxHashMap::default(),
            dirty_artifacts: FxHashSet::default(),
            record_contents: Vec::new(),
            record_name: record_name.to_owned(),
            record_dirty: true,
        };
        let mut header = vec![b'H'];
        header.extend_from_slice(metadata.as_bytes());
        let mut records = Vec::new();
        if resume {
            let mut offset = 0;
            let mut first = true;
            loop {
                let mut length = [0; 4];
                if let Err(error) = checkpoint.journal.read_exact(&mut length) {
                    if error.kind() != io::ErrorKind::UnexpectedEof {
                        return Err(error);
                    }
                    break;
                }
                let length = u32::from_le_bytes(length) as usize;
                if length == 0 || length > 1024 * 1024 {
                    return Err(invalid("invalid checkpoint frame length"));
                }
                let mut payload = vec![0; length];
                let mut check = [0; 8];
                let read = checkpoint
                    .journal
                    .read_exact(&mut payload)
                    .and_then(|_| checkpoint.journal.read_exact(&mut check));
                if let Err(error) = read {
                    if error.kind() != io::ErrorKind::UnexpectedEof {
                        return Err(error);
                    }
                    break;
                }
                if checksum(&payload) != u64::from_le_bytes(check) {
                    return Err(invalid("checkpoint checksum mismatch"));
                }
                if first {
                    if payload != header {
                        return Err(invalid(
                            "checkpoint settings/version do not match this search",
                        ));
                    }
                    first = false;
                } else {
                    match payload[0] {
                        b'D' => {
                            checkpoint.completed.insert(payload[1..].to_vec());
                        }
                        b'R' if payload.len() > 9 => {
                            let depth =
                                u64::from_le_bytes(payload[1..9].try_into().unwrap()) as usize;
                            if depth <= checkpoint.best {
                                return Err(invalid("checkpoint records are out of order"));
                            }
                            checkpoint.best = depth;
                            records.extend_from_slice(&payload[9..]);
                        }
                        b'A' if payload.len() >= 3 => {
                            let length =
                                u16::from_le_bytes(payload[1..3].try_into().unwrap()) as usize;
                            if payload.len() < 3 + length {
                                return Err(invalid("invalid checkpoint artifact"));
                            }
                            let name = std::str::from_utf8(&payload[3..3 + length])
                                .map_err(|_| invalid("invalid checkpoint artifact name"))?;
                            Self::validate_artifact_name(name)?;
                            checkpoint
                                .artifacts
                                .insert(name.to_owned(), payload[3 + length..].to_vec());
                        }
                        _ => return Err(invalid("invalid checkpoint event")),
                    }
                }
                offset += 4 + length as u64 + 8;
            }
            if first {
                return Err(invalid("checkpoint header is incomplete"));
            }
            // A crash may leave one incomplete final frame; replay that work.
            checkpoint.journal.set_len(offset)?;
            checkpoint.journal.seek(SeekFrom::Start(offset))?;
        } else {
            checkpoint.append(&header)?;
        }
        checkpoint.record_contents = records;
        checkpoint
            .dirty_artifacts
            .extend(checkpoint.artifacts.keys().cloned());
        checkpoint.refresh_exports();
        // This marker belongs to the previous invocation, never to the new one.
        match std::fs::remove_file(checkpoint.directory.join("STOP")) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(checkpoint)
    }

    fn append(&mut self, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 12);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(payload);
        frame.extend_from_slice(&checksum(payload).to_le_bytes());
        self.journal.write_all(&frame)?;
        self.journal.sync_all()
    }

    pub(crate) fn contains(&self, key: &[u8]) -> bool {
        self.completed.contains(key)
    }
    pub(crate) fn completed_count(&self) -> usize {
        self.completed.len()
    }

    pub(crate) fn complete(&mut self, key: Vec<u8>) -> io::Result<()> {
        let mut payload = vec![b'D'];
        payload.extend_from_slice(&key);
        self.append(&payload)?;
        self.completed.insert(key);
        self.refresh_exports();
        Ok(())
    }

    pub(crate) fn record(&mut self, depth: usize, text: &str) -> io::Result<()> {
        let mut payload = vec![b'R'];
        payload.extend_from_slice(&(depth as u64).to_le_bytes());
        payload.extend_from_slice(text.as_bytes());
        self.append(&payload)?;
        self.best = depth;
        self.record_contents.extend_from_slice(text.as_bytes());
        self.record_dirty = true;
        self.refresh_exports();
        Ok(())
    }

    fn validate_artifact_name(name: &str) -> io::Result<()> {
        if name.is_empty()
            || name.len() > u16::MAX as usize
            || Path::new(name)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(invalid("invalid checkpoint artifact path"));
        }
        Ok(())
    }

    fn write_artifact(&self, name: &str, data: &[u8]) -> io::Result<()> {
        let path = self.directory.join("search-records").join(name);
        std::fs::create_dir_all(path.parent().unwrap())?;
        File::create(path)?.write_all(data)
    }

    pub(crate) fn artifact(&self, name: &str) -> Option<&[u8]> {
        self.artifacts.get(name).map(Vec::as_slice)
    }

    pub(crate) fn artifacts(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.artifacts
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
    }

    fn refresh_exports(&mut self) {
        if self.record_dirty {
            let path = self.directory.join(&self.record_name);
            match File::create(&path).and_then(|mut file| file.write_all(&self.record_contents)) {
                Ok(()) => self.record_dirty = false,
                Err(error) => crate::search_log::warn_output(&path, &error),
            }
        }
        let pending: Vec<_> = self.dirty_artifacts.iter().cloned().collect();
        for name in pending {
            match self.write_artifact(&name, &self.artifacts[&name]) {
                Ok(()) => {
                    self.dirty_artifacts.remove(&name);
                }
                Err(error) => crate::search_log::warn_output(
                    &self.directory.join("search-records").join(&name),
                    &error,
                ),
            }
        }
    }

    pub(crate) fn put_artifact(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        Self::validate_artifact_name(name)?;
        let mut payload = vec![b'A'];
        payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(data);
        self.append(&payload)?;
        self.artifacts.insert(name.to_owned(), data.to_vec());
        self.dirty_artifacts.insert(name.to_owned());
        self.refresh_exports();
        Ok(())
    }
}

/// Exact root identity, independent of hash-map iteration and worker scheduling.
pub(crate) fn subtree_key(
    board: &crate::board::QuantumBoard,
    path: &[crate::types::Dir],
    walls: u128,
) -> Vec<u8> {
    use crate::types::Tile;
    let mut key = Vec::new();
    key.extend_from_slice(&walls.to_le_bytes());
    key.extend_from_slice(&(path.len() as u32).to_le_bytes());
    key.extend(path.iter().map(|dir| dir.idx() as u8));
    for (&tile, &mask) in board
        .tiles
        .iter()
        .zip(board.box_push_masks.iter())
        .take((board.width * board.height) as usize)
    {
        let (kind, id) = match tile {
            Tile::Wall => (0, 0),
            Tile::Floor => (1, 0),
            Tile::Box => (2, 0),
            Tile::Player => (3, 0),
            Tile::Quantum(id) => (4, id),
        };
        key.extend_from_slice(&[kind, id, mask]);
    }
    key.extend_from_slice(&(board.quantum_blocks.len() as u16).to_le_bytes());
    for qb in &board.quantum_blocks {
        key.extend_from_slice(&qb.wall_pos.unwrap_or(u16::MAX).to_le_bytes());
        let mut cells: Vec<_> = qb.cells.iter().collect();
        cells.sort_unstable_by_key(|(&pos, _)| pos);
        key.extend_from_slice(&(cells.len() as u16).to_le_bytes());
        for (&pos, starts) in cells {
            key.extend_from_slice(&pos.to_le_bytes());
            key.extend_from_slice(&(starts.len() as u16).to_le_bytes());
            for start in starts {
                key.extend_from_slice(&start.to_le_bytes());
            }
        }
        let mut masks: Vec<_> = qb.push_masks.iter().collect();
        masks.sort_unstable_by_key(|(&pos, _)| pos);
        key.extend_from_slice(&(masks.len() as u16).to_le_bytes());
        for (&pos, &mask) in masks {
            key.extend_from_slice(&pos.to_le_bytes());
            key.push(mask);
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scratch/tests-checkpoint")
            .join(format!("{}-{stamp}", std::process::id()))
    }

    #[cfg(windows)]
    #[test]
    fn locked_exports_do_not_block_proofs_or_resume_and_retry_after_unlock() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = directory();
        let mut checkpoint = Checkpoint::open(&dir, "settings", false, "records.txt").unwrap();
        checkpoint.record(22, "first record\n").unwrap();
        checkpoint
            .put_artifact("most-pushes.txt", b"score=99\n")
            .unwrap();
        let artifact_path = dir.join("search-records/most-pushes.txt");
        let artifact_viewer = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&artifact_path)
            .unwrap();
        let record_viewer = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(dir.join("records.txt"))
            .unwrap();
        checkpoint.record(23, "second record\n").unwrap();
        checkpoint
            .put_artifact("most-pushes.txt", b"score=100\n")
            .unwrap();
        checkpoint.complete(vec![1]).unwrap();
        assert_eq!(std::fs::read(&artifact_path).unwrap(), b"score=99\n");
        drop(checkpoint);
        let mut checkpoint = Checkpoint::open(&dir, "settings", true, "records.txt").unwrap();
        assert_eq!(checkpoint.best, 23);
        assert!(checkpoint.contains(&[1]));
        let records = crate::search_records::SearchRecords::new(
            dir.join("search-records"),
            Some(&checkpoint),
        )
        .unwrap();
        assert_eq!(
            checkpoint.artifact("most-pushes.txt"),
            Some(b"score=100\n".as_slice())
        );
        drop(records);
        drop(artifact_viewer);
        drop(record_viewer);
        checkpoint.complete(vec![2]).unwrap();
        assert_eq!(std::fs::read(&artifact_path).unwrap(), b"score=100\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("records.txt")).unwrap(),
            "first record\nsecond record\n"
        );
        // The journal itself is authoritative: its failures must still propagate.
        checkpoint.journal = File::open(dir.join("checkpoint.bin")).unwrap();
        assert!(checkpoint.complete(vec![3]).is_err());
        assert!(!checkpoint.contains(&[3]));
        assert!(checkpoint
            .put_artifact("most-pushes.txt", b"score=101\n")
            .is_err());
        assert_eq!(
            checkpoint.artifact("most-pushes.txt"),
            Some(b"score=100\n".as_slice())
        );
    }

    #[test]
    fn durable_records_exact_keys_locking_and_torn_tail_recovery() {
        let dir = directory();
        {
            let mut checkpoint = Checkpoint::open(&dir, "settings", false, "records.txt").unwrap();
            assert!(Checkpoint::open(&dir, "settings", true, "records.txt").is_err());
            checkpoint.record(22, "a saved record\n").unwrap();
            checkpoint.complete(vec![1, 2, 3]).unwrap();
            checkpoint
                .put_artifact("subtrees/test.txt", b"older snapshot")
                .unwrap();
            checkpoint
                .put_artifact("subtrees/test.txt", b"latest snapshot")
                .unwrap();
            assert!(checkpoint
                .put_artifact("../records.txt", b"invalid")
                .is_err());
            assert!(!checkpoint.contains(&[1, 2, 4]));
        }
        std::fs::write(dir.join("records.txt"), "incomplete external output").unwrap();
        std::fs::write(
            dir.join("search-records/subtrees/test.txt"),
            "incomplete external output",
        )
        .unwrap();
        let valid_length = std::fs::metadata(dir.join("checkpoint.bin")).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(dir.join("checkpoint.bin"))
            .unwrap()
            .write_all(&[10, 0, 0, 0, b'D', 1])
            .unwrap();
        let checkpoint = Checkpoint::open(&dir, "settings", true, "records.txt").unwrap();
        assert_eq!(checkpoint.best, 22);
        assert!(checkpoint.contains(&[1, 2, 3]));
        assert_eq!(checkpoint.completed_count(), 1);
        assert_eq!(
            checkpoint.artifact("subtrees/test.txt"),
            Some(b"latest snapshot".as_slice())
        );
        assert_eq!(
            std::fs::read(dir.join("search-records/subtrees/test.txt")).unwrap(),
            b"latest snapshot"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("records.txt")).unwrap(),
            "a saved record\n"
        );
        assert_eq!(
            std::fs::metadata(dir.join("checkpoint.bin")).unwrap().len(),
            valid_length
        );
        drop(checkpoint);
        assert!(Checkpoint::open(&dir, "other settings", true, "records.txt").is_err());
        assert!(Checkpoint::open(&dir, "settings", false, "records.txt").is_err());
        let mut journal = OpenOptions::new()
            .write(true)
            .open(dir.join("checkpoint.bin"))
            .unwrap();
        journal.seek(SeekFrom::Start(valid_length - 1)).unwrap();
        journal.write_all(&[0xff]).unwrap();
        drop(journal);
        assert!(Checkpoint::open(&dir, "settings", true, "records.txt").is_err());
    }

    #[test]
    fn subtree_identity_includes_masks_assignments_paths_and_wall_restrictions() {
        use crate::board::QuantumBoard;
        use crate::types::{Dir, QuantumBlock, Tile};
        let mut board = QuantumBoard::wall_filled_with_player(4, 4, 2, 2);
        let pos = board.flat(1, 1);
        board.tiles[pos as usize] = Tile::Quantum(0);
        board.quantum_blocks.push(QuantumBlock::concrete_box(pos));
        let key = subtree_key(&board, &[Dir::Up], 0);
        let mut other = board.clone();
        other.quantum_blocks[0]
            .push_masks
            .insert(pos, Dir::Up.mask());
        assert_ne!(key, subtree_key(&other, &[Dir::Up], 0));
        assert_ne!(key, subtree_key(&board, &[Dir::Down], 0));
        assert_ne!(key, subtree_key(&board, &[Dir::Up], 1));
        other = board.clone();
        other.box_push_masks[pos as usize] = 1;
        assert_ne!(key, subtree_key(&other, &[Dir::Up], 0));
        other = board.clone();
        other.quantum_blocks[0]
            .cells
            .get_mut(&pos)
            .unwrap()
            .push(pos + 1);
        assert_ne!(key, subtree_key(&other, &[Dir::Up], 0));
    }
}
