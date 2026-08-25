//! The policy floor (§4.3): config can tighten access, never loosen past a
//! compiled-in ceiling. Today this enforces the one PRD-explicit case —
//! credentials must never be reachable regardless of what any config layer
//! says — but the mechanism generalizes to more floors as subsystems land.

use valyria_types::Access;

use crate::error::{ConfigError, Result};
use crate::settings::Settings;

/// The maximum permissiveness any config layer may configure for a given
/// axis. `Access` is ordered least -> most permissive, so "at or under the
/// floor" is a plain `<=`.
pub struct PolicyFloor {
    pub max_credentials_access: Access,
}

impl Default for PolicyFloor {
    fn default() -> Self {
        Self {
            // Credentials are never exposed to the model or logs by
            // configuration alone — see D3/§29's redaction requirements.
            // A future "allow credential access for this one authorized
            // tool call" path is a permission *grant* (§22), not a global
            // config toggle, so the floor stays fixed at Denied.
            max_credentials_access: Access::Denied,
        }
    }
}

pub fn validate_floor(settings: &Settings, floor: &PolicyFloor) -> Result<()> {
    if settings.network.credentials > floor.max_credentials_access {
        return Err(ConfigError::PolicyFloorViolation {
            key: "network.credentials",
            configured: format!("{:?}", settings.network.credentials),
            floor: format!("{:?}", floor.max_credentials_access),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_pass_the_default_floor() {
        assert!(validate_floor(&Settings::default(), &PolicyFloor::default()).is_ok());
    }

    #[test]
    fn configuring_credentials_above_the_floor_is_rejected() {
        let mut settings = Settings::default();
        settings.network.credentials = Access::Allowed;
        let err = validate_floor(&settings, &PolicyFloor::default()).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::PolicyFloorViolation {
                key: "network.credentials",
                ..
            }
        ));
    }

    #[test]
    fn a_looser_floor_would_permit_it() {
        let mut settings = Settings::default();
        settings.network.credentials = Access::Controlled;
        let permissive_floor = PolicyFloor {
            max_credentials_access: Access::Allowed,
        };
        assert!(validate_floor(&settings, &permissive_floor).is_ok());
    }
}
