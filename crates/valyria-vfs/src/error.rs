use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("path escapes workspace root: {0}")]
    PathTraversal(String),
    #[error("symlink at {0} escapes the workspace root")]
    SymlinkEscape(String),
    #[error("watcher error: {0}")]
    Watch(String),
}

impl ErrorCode for VfsError {
    fn code(&self) -> &'static str {
        match self {
            VfsError::Io { .. } => "vfs.io",
            VfsError::PathTraversal(_) => "vfs.path_traversal",
            VfsError::SymlinkEscape(_) => "vfs.symlink_escape",
            VfsError::Watch(_) => "vfs.watch",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, VfsError>;
