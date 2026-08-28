//! The authority order (§4.18). Highest authority first; the rank is
//! explicit so the meaning survives a reordering of the enum, exactly as
//! `Trust::authority_rank` does in `valyria-types`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use valyria_types::Trust;

/// Where an instruction came from, which fixes both its position in the
/// merge order and its trust level.
///
/// ```text
/// 1. RuntimePolicy    (compiled in — not discovered here, listed for ordering)
/// 2. UserConfig       ~/.valyria/instructions.md
/// 3. WorkspaceValyria <root>/VALYRIA.md
/// 4. Agents           <root>/AGENTS.md
/// 5. Claude           <root>/CLAUDE.md
/// 6. DirectoryScoped  <dir>/{VALYRIA,AGENTS,CLAUDE}.md, nearest-to-file wins
/// 7. Advisory         <root>/{CONTRIBUTING.md, README[.md]} — parsed, never obeyed
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// The runtime's own compiled-in policy. `valyria-instructions` never
    /// produces this — it is here so [`Authority::rank`] covers the whole
    /// order — but `valyria-context` places its policy prompt at this rank.
    RuntimePolicy,
    /// `~/.valyria/instructions.md` (or wherever the user's global config
    /// lives): the operator's standing instructions across every workspace.
    UserConfig,
    /// `<root>/VALYRIA.md`: this runtime's dedicated workspace instruction
    /// file.
    WorkspaceValyria,
    /// `<root>/AGENTS.md`.
    Agents,
    /// `<root>/CLAUDE.md`.
    Claude,
    /// An instruction file inside a subdirectory. `depth` is how many
    /// directories below the workspace root it sits; a deeper file is
    /// *more* specific and wins over a shallower one within this tier.
    DirectoryScoped { dir: PathBuf, depth: usize },
    /// `CONTRIBUTING.md`, `README.md`, `README`. Advisory only: mined for
    /// conventions and commands and surfaced as repository *data*, never as
    /// a directive. Assigned [`Trust::RepoData`], not [`Trust::Instruction`].
    Advisory,
}

impl Authority {
    /// Lower rank = higher authority. Two `DirectoryScoped` files compare
    /// by depth (deeper first) so `neighbors` within the tier stay ordered.
    pub fn rank(&self) -> (u8, i64) {
        let tier = match self {
            Authority::RuntimePolicy => 0,
            Authority::UserConfig => 1,
            Authority::WorkspaceValyria => 2,
            Authority::Agents => 3,
            Authority::Claude => 4,
            Authority::DirectoryScoped { .. } => 5,
            Authority::Advisory => 6,
        };
        // Within the DirectoryScoped tier, a deeper file is more specific:
        // negate the depth so the natural `(tier, tiebreak)` ascending sort
        // puts it first.
        let tiebreak = match self {
            Authority::DirectoryScoped { depth, .. } => -(*depth as i64),
            _ => 0,
        };
        (tier, tiebreak)
    }

    /// The trust level content from this source carries into a prompt.
    /// Everything the runtime may *act on* is [`Trust::Instruction`];
    /// advisory files are [`Trust::RepoData`] — the same level as any
    /// other file in the repo, so prompt assembly fences them as data.
    pub fn trust(&self) -> Trust {
        match self {
            Authority::Advisory => Trust::RepoData,
            Authority::RuntimePolicy => Trust::Policy,
            _ => Trust::Instruction,
        }
    }

    /// A short stable label for logs and `--explain`.
    pub fn label(&self) -> String {
        match self {
            Authority::RuntimePolicy => "runtime-policy".to_string(),
            Authority::UserConfig => "user-config".to_string(),
            Authority::WorkspaceValyria => "workspace VALYRIA.md".to_string(),
            Authority::Agents => "AGENTS.md".to_string(),
            Authority::Claude => "CLAUDE.md".to_string(),
            Authority::DirectoryScoped { dir, depth } => {
                format!("directory-scoped ({}, depth {depth})", dir.display())
            }
            Authority::Advisory => "advisory".to_string(),
        }
    }

    /// Whether a source at this authority may be obeyed as an instruction
    /// (as opposed to merely mined for facts).
    pub fn is_directive(&self) -> bool {
        !matches!(self, Authority::Advisory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_order_is_user_then_valyria_then_agents_then_claude_then_scoped_then_advisory() {
        let mut all = vec![
            Authority::Advisory,
            Authority::Claude,
            Authority::UserConfig,
            Authority::Agents,
            Authority::WorkspaceValyria,
            Authority::DirectoryScoped {
                dir: "src".into(),
                depth: 1,
            },
        ];
        all.sort_by_key(|a| a.rank());
        assert_eq!(
            all,
            vec![
                Authority::UserConfig,
                Authority::WorkspaceValyria,
                Authority::Agents,
                Authority::Claude,
                Authority::DirectoryScoped {
                    dir: "src".into(),
                    depth: 1
                },
                Authority::Advisory,
            ]
        );
    }

    #[test]
    fn a_deeper_scoped_file_outranks_a_shallower_one() {
        let deep = Authority::DirectoryScoped {
            dir: "a/b/c".into(),
            depth: 3,
        };
        let shallow = Authority::DirectoryScoped {
            dir: "a".into(),
            depth: 1,
        };
        assert!(deep.rank() < shallow.rank());
    }

    #[test]
    fn advisory_is_repo_data_everything_else_is_instruction() {
        assert_eq!(Authority::Advisory.trust(), Trust::RepoData);
        assert_eq!(Authority::UserConfig.trust(), Trust::Instruction);
        assert_eq!(Authority::Claude.trust(), Trust::Instruction);
        assert_eq!(
            Authority::DirectoryScoped {
                dir: "x".into(),
                depth: 1
            }
            .trust(),
            Trust::Instruction
        );
    }

    #[test]
    fn only_advisory_is_non_directive() {
        assert!(!Authority::Advisory.is_directive());
        assert!(Authority::Agents.is_directive());
    }
}
