//! The permissive fallback (D10): used wherever real OS confinement isn't
//! implemented for this platform/configuration. Passes the command through
//! completely unmodified — and, critically, reports `Confinement::None`
//! rather than pretending otherwise.

use valyria_process::CommandSpec;

use crate::confinement::Confinement;
use crate::error::Result;
use crate::launcher::ProcessLauncher;
use crate::profile::SandboxProfile;

pub struct PermissiveSandbox;

impl ProcessLauncher for PermissiveSandbox {
    fn confinement_level(&self) -> Confinement {
        Confinement::None
    }

    fn wrap(&self, spec: CommandSpec, _profile: &SandboxProfile) -> Result<CommandSpec> {
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_no_confinement() {
        assert_eq!(PermissiveSandbox.confinement_level(), Confinement::None);
    }

    #[test]
    fn passes_the_command_through_unmodified() {
        let spec = CommandSpec::new("/bin/echo", "/tmp").arg("hi");
        let wrapped = PermissiveSandbox
            .wrap(spec.clone(), &SandboxProfile::new())
            .unwrap();
        assert_eq!(wrapped.program, spec.program);
        assert_eq!(wrapped.args, spec.args);
    }
}
