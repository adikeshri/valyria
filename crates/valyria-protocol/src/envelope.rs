//! Typed request/response dispatch (§4.27). The transport that carries
//! these — in-process (`valyria_app::EmbeddedClient`) or newline-delimited
//! JSON over a Unix socket ([`crate::transport`], the daemon) — is a pure
//! backend swap behind [`crate::Client`]; no call site changes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::messages::{
    CatalogRefreshRequest, CatalogRefreshResponse, ConfigSetRequest, ConfigShowResponse,
    DoctorRunResponse, Empty, EventsSubscribeRequest, GitBranchesResponse, GitDiffRequest,
    GitDiffResponse, GitLogRequest, GitLogResponse, GitStatusResponse, HardwareProbeResponse,
    HelloRequest, HelloResponse, IndexStatusResponse, LedgerChangesRequest, LedgerChangesResponse,
    MemoryListRequest, MemoryListResponse, ModelActivateRequest, ModelEndpointAddRequest,
    ModelEndpointListResponse, ModelIdRequest, ModelInspectResponse, ModelInstallRequest,
    ModelListResponse, ModelRecommendRequest, ModelRecommendResponse, ModelRemoveResponse,
    PermissionResolveRequest, PlanGetResponse, PlanRevisionsResponse, PurgeResponse,
    SearchQueryRequest, SearchQueryResponse, StorageInspectResponse, StoragePurgeRequest,
    TaskArtifactsResponse, TaskChildrenResponse, TaskCreateRequest, TaskCreateResponse,
    TaskIdRequest, TaskListResponse, TaskReportResponse, TaskRollbackRequest, TaskRollbackResponse,
    TaskStatusRequest, TaskStatusResponse, WireError, WorkspaceStatusResponse,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Hello(HelloRequest),
    TaskCreate(TaskCreateRequest),
    TaskStatus(TaskStatusRequest),
    TaskList(Empty),
    TaskReport(TaskIdRequest),
    TaskPlan(TaskIdRequest),
    /// Every direct child of a task, oldest first. Protocol 1.13.0 (M5).
    TaskChildren(TaskIdRequest),
    /// Every role-pipeline artifact produced against a task, oldest first.
    /// Protocol 1.13.0 (M5).
    TaskArtifacts(TaskIdRequest),
    /// Every plan revision for a task, oldest first, each with its diff
    /// against the immediately preceding one. Protocol 1.13.0 (M5).
    PlanRevisions(TaskIdRequest),
    TaskRollback(TaskRollbackRequest),
    TaskPause(TaskIdRequest),
    TaskResume(TaskIdRequest),
    TaskCancel(TaskIdRequest),
    PermissionResolve(PermissionResolveRequest),
    EventsSubscribe(EventsSubscribeRequest),
    WorkspaceStatus(Empty),
    DoctorRun(Empty),
    StorageInspect(Empty),
    StoragePurge(StoragePurgeRequest),
    ConfigShow(Empty),
    ConfigSet(ConfigSetRequest),
    MemoryList(MemoryListRequest),
    ModelList(Empty),
    GitStatus(Empty),
    GitDiff(GitDiffRequest),
    GitLog(GitLogRequest),
    GitBranches(Empty),
    SearchQuery(SearchQueryRequest),
    IndexStatus(Empty),
    /// Build (or rebuild) the whole-workspace index + graph so `search_query`
    /// and `index_status` have something to serve. Synchronous — the response
    /// carries the finished generation. Protocol 1.10.0.
    IndexBuild(Empty),
    HardwareProbe(Empty),
    ModelRecommend(ModelRecommendRequest),
    ModelInstall(ModelInstallRequest),
    /// Cancel an in-flight `model_install` for this id. The background task
    /// stops at its next checkpoint and emits `model_install_failed` with
    /// code `model_store.cancelled`; a `.part` file is kept so a later
    /// install resumes. Ack even when nothing was in flight. Protocol 1.11.0.
    ModelInstallCancel(ModelIdRequest),
    ModelRemove(ModelIdRequest),
    ModelActivate(ModelActivateRequest),
    ModelInspect(ModelIdRequest),
    /// Register an already-running external OpenAI-compatible server.
    /// Protocol 1.14.0 (M6).
    ModelEndpointAdd(ModelEndpointAddRequest),
    /// Unregister one; unbinds any role currently pointing at it.
    /// Protocol 1.14.0 (M6).
    ModelEndpointRemove(ModelIdRequest),
    /// List every registered external endpoint. Protocol 1.14.0 (M6).
    ModelEndpointList(Empty),
    /// Verify and, if newer, accept a signed catalog refresh. Protocol
    /// 1.15.0 (M6).
    CatalogRefresh(CatalogRefreshRequest),
    LedgerChanges(LedgerChangesRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum Response {
    Hello(HelloResponse),
    TaskCreate(TaskCreateResponse),
    TaskStatus(TaskStatusResponse),
    TaskList(TaskListResponse),
    TaskReport(TaskReportResponse),
    TaskPlan(PlanGetResponse),
    TaskChildren(TaskChildrenResponse),
    TaskArtifacts(TaskArtifactsResponse),
    PlanRevisions(PlanRevisionsResponse),
    TaskRollback(TaskRollbackResponse),
    WorkspaceStatus(WorkspaceStatusResponse),
    DoctorRun(DoctorRunResponse),
    StorageInspect(StorageInspectResponse),
    Purge(PurgeResponse),
    ConfigShow(ConfigShowResponse),
    MemoryList(MemoryListResponse),
    ModelList(ModelListResponse),
    GitStatus(GitStatusResponse),
    GitDiff(GitDiffResponse),
    GitLog(GitLogResponse),
    GitBranches(GitBranchesResponse),
    SearchQuery(SearchQueryResponse),
    IndexStatus(IndexStatusResponse),
    /// Result of [`Request::IndexBuild`] — the freshly built generation, same
    /// shape as [`Self::IndexStatus`]. Protocol 1.10.0.
    IndexBuild(IndexStatusResponse),
    HardwareProbe(HardwareProbeResponse),
    ModelRecommend(ModelRecommendResponse),
    ModelRemove(ModelRemoveResponse),
    ModelInspect(ModelInspectResponse),
    /// Protocol 1.14.0 (M6).
    ModelEndpointList(ModelEndpointListResponse),
    /// Protocol 1.15.0 (M6).
    CatalogRefresh(CatalogRefreshResponse),
    LedgerChanges(LedgerChangesResponse),
    /// A request that succeeded with nothing to return (`task.pause`,
    /// `task.resume`, `task.cancel`, `permission.resolve`,
    /// `model.install`, `model.install_cancel`, `model.activate`,
    /// `model.endpoint_add`, `model.endpoint_remove`).
    Ack,
    Error(WireError),
}

impl Response {
    pub fn is_error(&self) -> bool {
        matches!(self, Response::Error(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serde_round_trip_preserves_variant() {
        let req = Request::TaskCreate(TaskCreateRequest {
            objective: "add a function".into(),
            permission_mode: None,
        });
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    /// M5, protocol 1.13.0.
    #[test]
    fn multi_agent_request_and_response_variants_round_trip() {
        let req = Request::TaskChildren(TaskIdRequest {
            task_id: "task_01".into(),
        });
        assert_eq!(
            serde_json::to_value(&req).unwrap()["method"],
            "task_children"
        );
        let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(req, back);

        let resp = Response::TaskArtifacts(TaskArtifactsResponse {
            artifacts: vec![crate::messages::ArtifactWire {
                produced_by: "researcher".into(),
                kind: "research_brief".into(),
                artifact: serde_json::json!({"summary": "x"}),
                created_at_ms: 1,
            }],
        });
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(resp, back);

        let resp = Response::PlanRevisions(PlanRevisionsResponse { revisions: vec![] });
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn empty_param_variants_round_trip() {
        for req in [
            Request::TaskList(Empty {}),
            Request::DoctorRun(Empty {}),
            Request::StorageInspect(Empty {}),
            Request::ConfigShow(Empty {}),
            Request::ModelList(Empty {}),
            Request::WorkspaceStatus(Empty {}),
            Request::ModelEndpointList(Empty {}),
        ] {
            let json = serde_json::to_string(&req).unwrap();
            let back: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(req, back);
        }
    }

    /// M6: `model_endpoint_add/remove/list`, protocol 1.14.0.
    #[test]
    fn model_endpoint_request_and_response_variants_round_trip() {
        let req = Request::ModelEndpointAdd(ModelEndpointAddRequest {
            id: "ollama-local".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            display_name: Some("My Ollama".into()),
            remote_model_name: Some("qwen2.5-coder:7b".into()),
            context_length: Some(32768),
            supports_native_tools: Some(true),
            supports_grammar: Some(false),
        });
        assert_eq!(
            serde_json::to_value(&req).unwrap()["method"],
            "model_endpoint_add"
        );
        let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(req, back);

        // Every optional field is genuinely optional on the wire — an
        // older-shaped caller that only sends id/base_url still parses.
        let minimal = serde_json::json!({
            "method": "model_endpoint_add",
            "params": { "id": "x", "base_url": "http://localhost:1234" }
        });
        let parsed: Request = serde_json::from_value(minimal).unwrap();
        assert_eq!(
            parsed,
            Request::ModelEndpointAdd(ModelEndpointAddRequest {
                id: "x".into(),
                base_url: "http://localhost:1234".into(),
                display_name: None,
                remote_model_name: None,
                context_length: None,
                supports_native_tools: None,
                supports_grammar: None,
            })
        );

        let req = Request::ModelEndpointRemove(ModelIdRequest {
            id: "ollama-local".into(),
        });
        let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(req, back);

        let resp = Response::ModelEndpointList(ModelEndpointListResponse {
            endpoints: vec![crate::messages::ModelEndpointWire {
                id: "ollama-local".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                display_name: "My Ollama".into(),
                remote_model_name: "qwen2.5-coder:7b".into(),
                context_length: 32768,
                supports_native_tools: true,
                supports_grammar: false,
                created_at_ms: 1,
                active_roles: vec!["primary_coder".into()],
            }],
        });
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn response_error_variant_round_trips() {
        let resp = Response::Error(WireError {
            code: "task.not_found".into(),
            message: "no such task".into(),
            retryable: false,
        });
        assert!(resp.is_error());
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn ack_round_trips() {
        let resp = Response::Ack;
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
        assert!(!resp.is_error());
    }
}
