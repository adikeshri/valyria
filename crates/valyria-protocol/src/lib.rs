//! `valyria-protocol` — layer 6 (Interface).
//!
//! Versioned wire types and the `Client` boundary (§4.27, D11): the only
//! API surface `valyria-cli` (or any future desktop client) is allowed to
//! call.
//!
//! As of Phase 10 this is a **frozen v1 surface**: [`schema::export`]
//! renders the JSON Schema for [`Request`] / [`Response`] / [`WireEvent`]
//! into `docs/protocol/`, `xtask check-protocol` gates drift against
//! [`PROTOCOL_VERSION`] in CI, and [`transport::SocketClient`] implements
//! [`Client`] over a Unix socket so the daemon path is a pure backend swap.

#![forbid(unsafe_code)]

pub mod client;
pub mod envelope;
pub mod messages;
pub mod schema;
pub mod transport;
pub mod version;

pub use client::Client;
pub use envelope::{Request, Response};
pub use messages::{
    ConfigEntryWire, ConfigSetRequest, ConfigShowResponse, CpuInfoWire, DoctorCheckWire,
    DoctorRunResponse, Empty, EventsSubscribeRequest, GitBranchWire, GitBranchesResponse,
    GitCommitWire, GitDiffRequest, GitDiffResponse, GitFileStatusWire, GitLogRequest,
    GitLogResponse, GitStatusResponse, GpuInfoWire, HardwareProbeResponse, HelloRequest,
    HelloResponse, IndexStatusResponse, MemoryEntryWire, MemoryListRequest, MemoryListResponse,
    ModelActivateRequest, ModelCandidateWire, ModelIdRequest, ModelInspectResponse,
    ModelListResponse, ModelRecommendRequest, ModelRecommendResponse, ModelRemoveResponse,
    ModelSummaryWire, PermissionResolveRequest, PlanGetResponse, PlanStepSummary, PurgeResponse,
    ScoreExplanationWire, SearchFeatureWire, SearchHitWire, SearchQueryRequest,
    SearchQueryResponse, SearchStageScoreWire, StorageEntryWire, StorageInspectResponse,
    StoragePurgeRequest, TaskCreateRequest, TaskCreateResponse, TaskIdRequest, TaskListResponse,
    TaskReportResponse, TaskRollbackRequest, TaskRollbackResponse, TaskStatusRequest,
    TaskStatusResponse, TaskSummary, VerifiedClaimWire, WireError, WireEvent,
    WorkspaceStatusResponse,
};
pub use schema::{export as export_schema, SchemaFile};
pub use transport::{ClientFrame, ServerFrame, SocketClient};
pub use version::{capability, PROTOCOL_VERSION};
