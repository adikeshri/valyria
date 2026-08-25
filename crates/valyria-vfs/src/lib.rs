//! `valyria-vfs` — layer 1 (Platform).
//!
//! Workspace-rooted filesystem access (§4.4): every path resolution goes
//! through [`WorkspaceRoot::resolve`], which is the runtime's path
//! traversal and symlink-escape defense (§49). Also owns atomic writes
//! (D6), a stat-keyed content hash cache, binary/oversize classification,
//! `.gitignore`-aware traversal, and debounced filesystem watching.

#![forbid(unsafe_code)]

pub mod atomic;
pub mod classify;
pub mod error;
pub mod hash_cache;
pub mod list;
pub mod watcher;
pub mod workspace_path;

pub use atomic::write_atomic;
pub use classify::{is_oversized, looks_binary, looks_binary_file, DEFAULT_MAX_CONTEXT_FILE_BYTES};
pub use error::{Result, VfsError};
pub use hash_cache::HashCache;
pub use list::list_files;
pub use watcher::{ChangeSet, Watcher};
pub use workspace_path::WorkspaceRoot;
