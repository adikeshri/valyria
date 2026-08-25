//! Confinement reporting (§21, D10): the runtime must always know and be
//! able to state exactly what protection a sandboxed run actually got —
//! never silently degrade from "confined" to "not confined".

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Confinement {
    /// No OS-level confinement at all — `PermissiveSandbox`, or a
    /// platform/configuration where real confinement isn't implemented
    /// yet. The runtime and `doctor` must surface this, not hide it.
    None,
    Filesystem,
    FilesystemAndNetwork,
    FilesystemNetworkAndResource,
}

impl std::fmt::Display for Confinement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Confinement::None => "none",
            Confinement::Filesystem => "filesystem",
            Confinement::FilesystemAndNetwork => "filesystem+network",
            Confinement::FilesystemNetworkAndResource => "filesystem+network+resource",
        };
        f.write_str(s)
    }
}
