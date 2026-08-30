//! Semantic versioning of the protocol itself (§4.27), independent of
//! `valyria`'s own crate version. `Hello` negotiates this.
//!
//! **This is a frozen surface as of Phase 10.** `xtask schema` exports the
//! JSON Schema for [`crate::Request`] / [`crate::Response`] / [`crate::
//! WireEvent`] into `docs/protocol/`, and `xtask check-protocol` (a CI
//! gate) fails any change to those schemas that did not also change this
//! constant. Bump it deliberately:
//!
//! - **patch** — a new event `kind`/`payload` shape, doc-only changes;
//! - **minor** — a new `Request`/`Response` variant or a new optional
//!   field (backward compatible: old clients ignore it);
//! - **major** — a removed/renamed variant or field, or a changed type
//!   (breaking: old clients misparse).
pub const PROTOCOL_VERSION: &str = "1.2.0";

/// Capability tokens a `HelloResponse` advertises (§4.27). A client
/// negotiates against these, not the version string — a runtime built
/// without the `daemon` feature simply omits `"daemon"`.
pub mod capability {
    pub const PLAN: &str = "plan";
    pub const DOCTOR: &str = "doctor";
    pub const STORAGE: &str = "storage";
    pub const MEMORY: &str = "memory";
    pub const MODELS: &str = "models";
    pub const ROLLBACK: &str = "rollback";
    pub const EVENTS_RESUME: &str = "events_resume";
    /// `config_set` is served: a client may write Core-owned config leaves
    /// (policy-floor validated) rather than editing `config.toml` itself.
    pub const CONFIG_WRITE: &str = "config_write";
    /// `task_create` accepts a per-task `permission_mode` override, so the
    /// autonomy control need not restart the daemon (§25).
    pub const TASK_PERMISSION_MODE: &str = "task_permission_mode";
    /// The read-only repository surface is served: `git_status`,
    /// `git_diff`, `git_log`, `git_branches`, `search_query`,
    /// `index_status` (§7, §14, §17, §33). A client with this capability
    /// stops using its local-read fallback for these.
    pub const REPO: &str = "repo";

    /// The full set an embedded runtime supports.
    pub const ALL: &[&str] = &[
        PLAN,
        DOCTOR,
        STORAGE,
        MEMORY,
        MODELS,
        ROLLBACK,
        EVENTS_RESUME,
        CONFIG_WRITE,
        TASK_PERMISSION_MODE,
        REPO,
    ];
}
