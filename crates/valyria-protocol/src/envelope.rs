//! Typed request/response dispatch (§4.27). The transport that carries
//! these — in-process (`valyria_app::EmbeddedClient`) or newline-delimited
//! JSON over a Unix socket ([`crate::transport`], the daemon) — is a pure
//! backend swap behind [`crate::Client`]; no call site changes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::messages::{
    ConfigSetRequest, ConfigShowResponse, DoctorRunResponse, Empty, EventsSubscribeRequest,
    GitBranchesResponse, GitDiffRequest, GitDiffResponse, GitLogRequest, GitLogResponse,
    GitStatusResponse, HardwareProbeResponse, HelloRequest, HelloResponse, IndexStatusResponse,
    LedgerChangesRequest, LedgerChangesResponse, MemoryListRequest, MemoryListResponse,
    ModelActivateRequest, ModelIdRequest, ModelInspectResponse, ModelListResponse,
    ModelRecommendRequest, ModelRecommendResponse, ModelRemoveResponse, PermissionResolveRequest,
    PlanGetResponse, PurgeResponse, SearchQueryRequest, SearchQueryResponse,
    StorageInspectResponse, StoragePurgeRequest, TaskCreateRequest, TaskCreateResponse,
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
    ModelInstall(ModelIdRequest),
    ModelRemove(ModelIdRequest),
    ModelActivate(ModelActivateRequest),
    ModelInspect(ModelIdRequest),
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
    LedgerChanges(LedgerChangesResponse),
    /// A request that succeeded with nothing to return (`task.pause`,
    /// `task.resume`, `task.cancel`, `permission.resolve`,
    /// `model.install`, `model.activate`).
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

    #[test]
    fn empty_param_variants_round_trip() {
        for req in [
            Request::TaskList(Empty {}),
            Request::DoctorRun(Empty {}),
            Request::StorageInspect(Empty {}),
            Request::ConfigShow(Empty {}),
            Request::ModelList(Empty {}),
            Request::WorkspaceStatus(Empty {}),
        ] {
            let json = serde_json::to_string(&req).unwrap();
            let back: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(req, back);
        }
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
