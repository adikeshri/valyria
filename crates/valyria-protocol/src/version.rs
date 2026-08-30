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
pub const PROTOCOL_VERSION: &str = "1.6.0";

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
    /// `hardware_probe` and `model_recommend` are served — a structured
    /// hardware report and Core's `fit()`-scored model recommendation
    /// (§22, §37), so the client need not invent a heuristic.
    pub const HARDWARE: &str = "hardware";
    /// The model lifecycle is served, not just `model_list`:
    /// `model_install` (with `model_install_progress` events),
    /// `model_remove`, `model_activate`, `model_inspect` (§20, §21).
    pub const MODEL_MANAGE: &str = "model_manage";
    /// Context provenance is emitted: a `context_retrieved` event per
    /// Discovery step, carrying the retrieved items with reason / trust /
    /// tokens and the budget used (§34). The Context Inspector lights up.
    pub const CONTEXT: &str = "context";
    /// `ledger_changes { task_id }` is served — `valyria-ledger`'s
    /// agent-authored / pre-existing / concurrent-user classification for
    /// the diff viewer's ownership column (§15, §16).
    pub const LEDGER: &str = "ledger";
    /// Fine-grained diagnostics are present: a discoverable
    /// `checkpoint_id` on `PlanStepSummary` plus a `plan_checkpoint`
    /// event (G13); a `tool_invocation_id` pairing `tool_started` with
    /// `tool_completed` and structured `{exit_code, stdout, stderr,
    /// duration_ms}` on the latter (G14); a parsed `location[]` on
    /// `test_failed` / `verification_evidence` (G15).
    pub const DIAGNOSTICS_V2: &str = "diagnostics_v2";
    /// The daemon authenticates local clients (G10): every connection is
    /// peer-uid checked against the daemon's own OS user, and — when the
    /// daemon was started with a token — every frame must be an
    /// `AuthCall` / `AuthSubscribe` carrying it.
    pub const CLIENT_AUTH: &str = "client_auth";

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
        HARDWARE,
        MODEL_MANAGE,
        CONTEXT,
        LEDGER,
        DIAGNOSTICS_V2,
        CLIENT_AUTH,
    ];
}
