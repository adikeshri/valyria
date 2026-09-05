//! Wire message shapes (§4.27). IDs cross the wire as their `Display`
//! strings (`task_01H...`), never bare ULIDs — the same convention
//! `valyria_types::id` uses internally, kept consistent at the protocol
//! boundary so a client never has to know about the prefix scheme to
//! round-trip an id it was handed.
//!
//! Every type here derives [`schemars::JsonSchema`] so `xtask schema` can
//! export the wire contract and the CI compat gate can fail a breaking
//! change that did not also bump [`crate::PROTOCOL_VERSION`] (§4.27, D11).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HelloRequest {
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResponse {
    pub protocol_version: String,
    pub runtime_version: String,
    /// What this runtime build can do — a client negotiates against this
    /// rather than the version string. Names are stable identifiers
    /// (`plan`, `daemon`, `doctor`, …); unknown names are ignored.
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskCreateRequest {
    pub objective: String,
    /// Optional per-task autonomy override (§25). One of `manual` |
    /// `assisted` | `autonomous`. When absent, the task inherits the
    /// daemon's start-time mode. Additive as of protocol 1.1.0 — an older
    /// client omits it and gets the daemon default, unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskCreateResponse {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskIdRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskStatusRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskStatusResponse {
    pub task_id: String,
    pub objective: String,
    pub state: String,
    pub paused_from: Option<String>,
    pub recovery_note: Option<String>,
}

/// An empty request payload. Kept as a named unit struct (rather than
/// `()`) so it has a stable schema name and a place to grow fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Empty {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskSummary {
    pub task_id: String,
    pub objective: String,
    pub state: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskListResponse {
    pub tasks: Vec<TaskSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VerifiedClaimWire {
    pub kind: String,
    pub command: String,
    pub outcome: String,
    pub run_id: String,
}

/// Mirrors `valyria_verify::CompletionReport` (§15, D4) — assembled only
/// from persisted verification runs, so an unbacked "tests pass" shows up
/// in `unverified`, never `verified`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskReportResponse {
    pub task_id: String,
    pub status: String,
    pub verified: Vec<VerifiedClaimWire>,
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRollbackRequest {
    pub task_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRollbackResponse {
    pub reverted_entries: u64,
    pub restored_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanStepSummary {
    pub id: String,
    pub intent: String,
    pub targets: Vec<String>,
    pub depends_on: Vec<String>,
    pub rollback_boundary: bool,
    pub checkpoint: bool,
    /// The `checkpoint_id` a checkpoint at this step was recorded under,
    /// when one exists — the id `task_rollback` expects (§16, G13).
    /// Additive as of protocol 1.5.0; also emitted live as a
    /// `plan_checkpoint` event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanGetResponse {
    /// `None` when the task ran as the pass-through (no model-authored
    /// plan) — not an error.
    pub revision: Option<u32>,
    pub content_hash: Option<String>,
    pub steps: Vec<PlanStepSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionResolveRequest {
    pub task_id: String,
    /// Legacy approve/deny (protocol 1.0). Ignored when `decision` is set.
    pub approve: bool,
    /// The `request_id` from the `approval_requested` payload (G2). When
    /// present it is asserted against the daemon's current pending
    /// request; a stale id is rejected with `approval.superseded` rather
    /// than resolving the wrong prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `once` (approve this call), `task` (approve and grant for the rest
    /// of the task — "Allow for Task"), or `deny`. Overrides `approve`
    /// when set. Additive as of protocol 1.8.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventsSubscribeRequest {
    pub since: u64,
    /// Restrict the stream to this task's events plus workspace-global
    /// (task-less) events (protocol 1.7.0, G11). Absent = the full stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

// --- doctor (§4.28) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheckWire {
    pub name: String,
    /// `pass` | `warn` | `fail`.
    pub status: String,
    pub detail: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorRunResponse {
    pub checks: Vec<DoctorCheckWire>,
    /// The worst status across all checks: `pass` | `warn` | `fail`.
    pub summary: String,
}

// --- storage inspection / clean (§4.1, §48) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StorageEntryWire {
    /// e.g. `workspace.db`, `blobs`, `index`, `tasks`, `logs`, `models`.
    pub name: String,
    pub bytes: u64,
    pub detail: Option<String>,
    /// Whether `storage.purge` can reclaim this entry.
    pub purgeable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StorageInspectResponse {
    pub entries: Vec<StorageEntryWire>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StoragePurgeRequest {
    /// `memory` | `cache` | `tasks` | `logs`.
    pub scope: String,
    /// When true, report what *would* be freed without deleting anything.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PurgeResponse {
    pub freed_bytes: u64,
    pub items_removed: u64,
    pub dry_run: bool,
}

// --- config (§4.3) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigEntryWire {
    pub key: String,
    pub value: String,
    /// Where the effective value came from: `default` | `global` |
    /// `workspace` | `env` | `task`.
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigShowResponse {
    pub entries: Vec<ConfigEntryWire>,
}

/// Write one dotted config leaf to a Core-owned config file, then report
/// the re-resolved effective view (§24). Additive as of protocol 1.1.0.
///
/// The write is validated against the policy floor
/// (`valyria_config::validate_floor`) *before* it touches disk: a value
/// that would loosen access past the compiled-in ceiling is rejected with
/// `config.policy_floor_violation` and nothing is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSetRequest {
    /// Dotted key, e.g. `permission.mode`, `log.format`,
    /// `network.internet`. Must be a key `config_show` already reports.
    pub key: String,
    /// The new value, as the string form `config_show` would display.
    pub value: String,
    /// `workspace` writes `<repo>/.valyria/config.toml`; `user` writes
    /// `~/.valyria/config.toml`. Anything else is `config.invalid_scope`.
    pub scope: String,
}

// --- memory (§4.19, §32) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryListRequest {
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryEntryWire {
    pub id: String,
    pub kind: String,
    pub scope: String,
    pub author: String,
    pub text: String,
    pub effective_confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryListResponse {
    pub entries: Vec<MemoryEntryWire>,
}

// --- models (§4.21) ---

/// One row of `model_list` — a catalog card joined with local state. The
/// catalog ships embedded, so `model_list` is the full "what can I run"
/// surface (not just what is installed): the client's model manager and
/// first-run "set up a model" step read it directly, and `installed` /
/// `active_roles` say what is on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelSummaryWire {
    pub id: String,
    pub family: String,
    /// Human-facing name, e.g. `Qwen2.5-Coder 7B Instruct (Q4_K_M)`.
    pub display_name: String,
    pub quantization: String,
    /// Parameter count in billions (`7.0`, `1.5`, `0.137`).
    pub parameters_b: f64,
    /// Maximum context window the weights support.
    pub context_length: u32,
    pub size_bytes: u64,
    pub installed: bool,
    pub license: String,
    /// `ModelRole` names this model is currently bound to (e.g.
    /// `["primary_coder", "planner"]`). Empty when it serves no role.
    /// The client reads this instead of guessing the active model from
    /// config keys.
    #[serde(default)]
    pub active_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelListResponse {
    pub models: Vec<ModelSummaryWire>,
}

// --- repository read surface (§7, §14, §17, §33; capability `repo`) ---
//
// Additive as of protocol 1.2.0. Every method here is read-only: the app's
// diff viewer, changed-file rail, code search and git panel stop being
// served by a local-read fallback and are served by Core instead. Git
// *writes* remain Core-internal and are not exposed.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitFileStatusWire {
    pub path: String,
    /// `added` | `modified` | `deleted` | `untracked` | `conflicted`.
    pub kind: String,
    /// `true` for an index-vs-HEAD (staged) entry, `false` for a
    /// worktree-vs-index (unstaged / untracked) entry.
    pub staged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitStatusResponse {
    /// Current branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
    pub detached: bool,
    /// HEAD commit SHA, or `None` for an unborn HEAD.
    pub head_commit: Option<String>,
    pub files: Vec<GitFileStatusWire>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitDiffRequest {
    /// Restrict the diff to this exact repo-relative path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `false` → worktree vs index (`git diff`); `true` → index vs HEAD
    /// (`git diff --staged`).
    #[serde(default)]
    pub staged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitDiffResponse {
    /// Unified-diff text; empty when there is nothing to show.
    pub unified: String,
    /// `true` when `unified` was clipped at Core's size cap.
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitLogRequest {
    /// Newest-first commits from HEAD. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitCommitWire {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    /// The commit subject (first line).
    pub message: String,
    /// Author time, unix seconds.
    pub time_unix: i64,
    pub parents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitLogResponse {
    pub commits: Vec<GitCommitWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitBranchWire {
    pub name: String,
    pub commit: String,
    pub is_head: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitBranchesResponse {
    pub branches: Vec<GitBranchWire>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchQueryRequest {
    /// The query phrase, or a pattern for `regex` / `ast` modes.
    pub query: String,
    /// Mode names (`lexical`, `symbol`, `semantic`, `regex`, `ast`,
    /// `dependency`, `git`). Empty runs the engine's default set. An
    /// unknown name is `search.unknown_mode`.
    #[serde(default)]
    pub modes: Vec<String>,
    /// Files the current task is anchored on — they seed dependency-mode
    /// traversal and pull nearby files up the ranking.
    #[serde(default)]
    pub anchors: Vec<String>,
    /// Max hits. Defaults to 20, capped at 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One reranking feature and its weighted contribution — mirrors
/// `valyria_search::Feature`. The features of a hit sum *exactly* to its
/// `score` (§14: "why this file?" answered from stored data).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchFeatureWire {
    pub name: String,
    pub value: f64,
    pub weight: f64,
    pub contribution: f64,
}

/// How one retrieval mode ranked a hit before fusion — mirrors
/// `valyria_search::StageScore`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchStageScoreWire {
    pub mode: String,
    pub rank: u32,
    pub raw_score: f64,
}

/// The full derivation of a hit's score — mirrors
/// `valyria_search::ScoreExplanation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScoreExplanationWire {
    pub stage_scores: Vec<SearchStageScoreWire>,
    pub features: Vec<SearchFeatureWire>,
    pub retrieval_paths: Vec<String>,
}

/// One ranked hit — mirrors `valyria_search::SearchHit`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchHitWire {
    pub path: String,
    pub symbol_path: Option<String>,
    pub line: Option<u32>,
    pub snippet: Option<String>,
    pub score: f64,
    pub explanation: ScoreExplanationWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchQueryResponse {
    pub hits: Vec<SearchHitWire>,
    /// Modes that actually ran.
    pub modes_run: Vec<String>,
    /// Human-readable notes about modes that stepped aside ("semantic: no
    /// embeddings for generation 3", "git: not a git repository"). Never
    /// an error — a missing mode degrades the result, it does not fail it.
    pub degraded: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IndexStatusResponse {
    /// `None` when the workspace has never been indexed.
    pub generation: Option<u64>,
    /// `files` | `complete` (the generation's `GenerationStage`).
    pub stage: Option<String>,
    pub file_count: u64,
    pub symbol_count: u64,
    /// When the current generation was published, unix milliseconds.
    pub created_at_ms: Option<i64>,
}

// --- hardware & models (§20, §21, §22, §37; capabilities `hardware`,
// `models`) ---
//
// Additive as of protocol 1.3.0. `hardware_probe` and `model_recommend`
// give the first-run wizard a structured source and let it *explain* a
// recommendation from Core's `fit()` scoring rather than a heuristic
// (§41). `model_install` / `_remove` / `_activate` / `_inspect` drive
// `valyria-model-store`; install returns immediately and reports progress
// on the event stream (`model_install_progress` / `_completed` /
// `_failed`).

/// Mirrors `valyria_hardware::CpuInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CpuInfoWire {
    pub brand: String,
    pub physical_cores: u32,
    pub logical_cores: u32,
    pub arch: String,
}

/// Mirrors `valyria_hardware::GpuInfo`. `vram_bytes` is `None` on a
/// unified-memory system — meaningful, not missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GpuInfoWire {
    pub name: String,
    pub vendor: Option<String>,
    pub core_count: Option<u32>,
    pub vram_bytes: Option<u64>,
}

/// Mirrors `valyria_hardware::HardwareReport` (§37, §39).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HardwareProbeResponse {
    pub os: String,
    pub os_version: Option<String>,
    pub arch: String,
    pub cpu: CpuInfoWire,
    pub ram_total_bytes: u64,
    pub ram_available_bytes: u64,
    pub gpus: Vec<GpuInfoWire>,
    /// CPU and GPU share one memory pool (Apple Silicon today).
    pub unified_memory: bool,
    /// `Some(false)` = probed and absent; `None` = not probed on this
    /// platform yet.
    pub accelerator_present: Option<bool>,
    pub disk_total_bytes: u64,
    pub disk_available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRecommendRequest {
    /// A `ModelRole` name: `primary_coder`, `fast_coder`, `planner`,
    /// `reviewer`, `embedder`, `reranker`, `autocomplete`, `summarizer`.
    pub role: String,
}

/// One scored candidate for a role on this machine — mirrors
/// `valyria_model_registry::CardScore` joined with its card. A card that
/// will not fit is still listed, with `fit_kind = "will_not_fit"` and no
/// `adjusted_score`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelCandidateWire {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub size_bytes: u64,
    pub license_name: String,
    pub installed: bool,
    /// Catalog role suitability, 0..=100.
    pub suitability: u32,
    /// `comfortable` | `tight` | `will_not_fit`.
    pub fit_kind: String,
    /// For `tight`, the estimated resource utilisation (0.0..~1.0); for
    /// `will_not_fit`, the reason (`insufficient_ram` | `insufficient_vram`).
    pub fit_detail: Option<String>,
    /// `suitability` minus the tight-fit penalty — the value Core ranks
    /// on. `None` when the card will not fit.
    pub adjusted_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRecommendResponse {
    pub role: String,
    /// The best-scoring fitting candidate, if any.
    pub recommended: Option<ModelCandidateWire>,
    /// Every candidate for the role, best first (non-fitting ones sorted
    /// last).
    pub candidates: Vec<ModelCandidateWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelIdRequest {
    pub id: String,
}

/// `model_install` — begin a download. `accept_license` is the wire record
/// of the user's acceptance of the model's license (its text is on
/// `ModelInspectResponse::license_text`). Core **refuses** the install with
/// `model.license_not_accepted` when it is `false`, so no weights are ever
/// fetched without an explicit acknowledgement (§4.21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelInstallRequest {
    pub id: String,
    #[serde(default)]
    pub accept_license: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelActivateRequest {
    pub id: String,
    /// The role to bind `id` to (a `ModelRole` name).
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRemoveResponse {
    pub freed_bytes: u64,
}

/// Full detail for one model — mirrors the `ModelCard` plus, when
/// installed, its `manifest.json` (§4.21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelInspectResponse {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub parameters_b: f64,
    pub quantization: String,
    pub context_length: u32,
    pub size_bytes: u64,
    pub license_name: String,
    pub license_url: Option<String>,
    /// The full license body when Core bundles it locally (it does for
    /// every catalog model), for the install acceptance prompt. `None`
    /// falls back to `license_url`.
    pub license_text: Option<String>,
    /// Unix ms at which the user accepted this model's license, from its
    /// install manifest. `None` when not installed, or installed before
    /// the acceptance step existed.
    pub license_accepted_at_ms: Option<i64>,
    pub source_url: String,
    pub installed: bool,
    /// Present only when installed.
    pub installed_at_ms: Option<i64>,
    /// Measured decode throughput from the post-install probe, if recorded.
    pub probe_tokens_per_sec: Option<f64>,
    /// The roles this model is currently bound to.
    pub active_roles: Vec<String>,
}

// --- change ownership (§15, §16; capability `ledger`) ---
//
// Additive as of protocol 1.4.0. Surfaces `valyria-ledger`'s
// agent-authored / pre-existing / concurrent-user classification for the
// diff viewer's ownership column. Context provenance (G7) has no request
// — it is the `context_retrieved` *event*, emitted per Discovery step.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LedgerChangesRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LedgerChangeWire {
    pub path: String,
    /// `agent_authored` | `pre_existing` | `concurrent_user_modification`
    /// | `unknown` — computed against the file's on-disk state now.
    pub classification: String,
    /// `write` | `delete` — the agent's most recent action on the path.
    pub kind: String,
    pub task_id: String,
    pub step_id: String,
    pub tool_invocation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LedgerChangesResponse {
    /// One row per agent-touched path, path-ordered.
    pub changes: Vec<LedgerChangeWire>,
}

// --- workspace ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceStatusResponse {
    pub workspace_id: String,
    pub root: String,
    pub data_dir: String,
    pub index_generation: Option<u64>,
    pub active_tasks: u32,
    pub total_tasks: u32,
}

/// Mirrors `valyria_types::CodedError` at the wire boundary — every
/// `ErrorCode`-implementing error in the runtime reduces to this shape
/// before crossing the protocol, matching §3's "errors that reach the
/// model [and the client] are converted through a redaction pass first"
/// convention (redaction itself is a later phase; the shape is stable now).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WireError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// A projected event (§43), independent of transport. `kind`/`payload`
/// mirror `valyria_events::EventEnvelope` — deliberately loose-typed
/// (`kind` as a string, `payload` as raw JSON) since new event kinds and
/// payload shapes are added by many crates over the life of the project,
/// and the wire schema shouldn't need a breaking change every time one is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WireEvent {
    pub seq: u64,
    pub task_id: Option<String>,
    pub ts_ms: u128,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_event_serde_round_trip() {
        let event = WireEvent {
            seq: 1,
            task_id: Some("task_01H".into()),
            ts_ms: 1000,
            kind: "state_changed".into(),
            payload: serde_json::json!({"from": "IDLE", "to": "UNDERSTANDING"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn permission_resolve_request_round_trip() {
        // Legacy shape (no request_id / decision) still parses.
        let legacy: PermissionResolveRequest =
            serde_json::from_str(r#"{"task_id":"task_01H","approve":true}"#).unwrap();
        assert_eq!(legacy.request_id, None);
        assert_eq!(legacy.decision, None);

        let req = PermissionResolveRequest {
            task_id: "task_01H".into(),
            approve: true,
            request_id: Some("eff_01H".into()),
            decision: Some("task".into()),
        };
        let back: PermissionResolveRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn storage_purge_request_dry_run_defaults_false() {
        let req: StoragePurgeRequest = serde_json::from_str(r#"{"scope":"cache"}"#).unwrap();
        assert!(!req.dry_run);
    }

    #[test]
    fn task_create_request_permission_mode_defaults_none_and_is_omitted() {
        // An older client that sends only `objective` still parses.
        let req: TaskCreateRequest =
            serde_json::from_str(r#"{"objective":"add a function"}"#).unwrap();
        assert_eq!(req.permission_mode, None);
        // And a None mode does not serialize a null key back onto the wire.
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"objective":"add a function"}"#);
    }

    #[test]
    fn task_create_request_carries_permission_mode_when_set() {
        let req = TaskCreateRequest {
            objective: "x".into(),
            permission_mode: Some("manual".into()),
        };
        let back: TaskCreateRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn config_set_request_round_trips() {
        let req = ConfigSetRequest {
            key: "log.format".into(),
            value: "json".into(),
            scope: "workspace".into(),
        };
        let back: ConfigSetRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }
}
