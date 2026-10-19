// lib.rs — module declarations for the quantum crate.

pub mod backward;
mod backward_deadlock;
pub mod bfs;
pub mod bfsrelaxed;
pub mod board;
pub mod deadlock;
pub mod io;
pub mod qstate;
mod qb_split;
#[cfg(feature = "profile-search")]
pub mod search_profile;
pub mod types;
#[cfg(any(feature = "indexed-dominance", test))]
mod dominance;
pub mod shortcut_paths;
mod progress;
mod search_log;
mod work_queue;
mod checkpoint;
mod symmetry;
mod search_records;
#[cfg(any(feature = "walking-dominance", test))]
mod walking_dominance;
