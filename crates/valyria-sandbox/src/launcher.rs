//! The `ProcessLauncher` trait (§21, D10): transforms a [`CommandSpec`]
//! so that when it's executed (by `valyria-process::run`), it runs under
//! this launcher's confinement — and always knows and reports what level
//! of confinement that actually is.

use valyria_process::CommandSpec;

use crate::confinement::Confinement;
use crate::error::Result;
use crate::profile::SandboxProfile;

pub trait ProcessLauncher: Send + Sync {
    /// What this launcher actually achieves on the current platform —
    /// never a claim, always the measured/known truth.
    fn confinement_level(&self) -> Confinement;

    /// Wrap `spec` so it runs under `profile`'s constraints. May change
    /// `program`/`args` entirely (e.g. prepending `sandbox-exec -p ...`)
    /// while leaving `cwd`/`env`/timeouts untouched.
    fn wrap(&self, spec: CommandSpec, profile: &SandboxProfile) -> Result<CommandSpec>;
}

/// Picks the best launcher this platform actually supports, verified
/// rather than assumed (e.g. checks that `sandbox-exec` really exists at
/// its expected path on macOS before claiming filesystem confinement).
pub fn detect_platform_launcher() -> Box<dyn ProcessLauncher> {
    #[cfg(target_os = "macos")]
    {
        if let Some(launcher) = crate::macos::SeatbeltLauncher::detect() {
            return Box::new(launcher);
        }
    }
    Box::new(crate::permissive::PermissiveSandbox)
}
