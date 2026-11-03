// lib.rs — module declarations for the quantum crate.

pub mod backward;
pub mod backward_deadlock;
pub mod bfs;
pub mod bfsrelaxed;
pub mod board;
pub mod deadlock;
pub mod io;
pub mod qstate;
pub mod qb_split;
#[cfg(feature = "profile-search")]
pub mod search_profile;
pub mod types;
#[cfg(any(feature = "indexed-dominance", test))]
pub mod dominance;
pub mod shortcut_paths;
pub mod progress;
pub mod search_log;
pub mod work_queue;
pub mod checkpoint;
pub mod symmetry;
pub mod search_records;
#[cfg(any(feature = "walking-dominance", test))]
pub mod walking_dominance;
