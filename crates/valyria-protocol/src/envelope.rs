//! Typed request/response dispatch (§4.27). Full JSON-RPC 2.0 framing +
//! `xtask schema` export + the compat-CI-gate is Phase 10; Phase 3 needs
//! only this typed enum and the [`crate::client::Client`] boundary to be
//! real, so a daemon transport can be added later as a pure backend swap
//! rather than a call-site change.

use serde::{Deserialize, Serialize};

use crate::messages::{
    EventsSubscribeRequest, HelloRequest, HelloResponse, PermissionResolveRequest,
    TaskCreateRequest, TaskCreateResponse, TaskIdRequest, TaskStatusRequest, TaskStatusResponse,
    WireError,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Hello(HelloRequest),
    TaskCreate(TaskCreateRequest),
    TaskStatus(TaskStatusRequest),
    TaskPause(TaskIdRequest),
    TaskResume(TaskIdRequest),
    TaskCancel(TaskIdRequest),
    PermissionResolve(PermissionResolveRequest),
    EventsSubscribe(EventsSubscribeRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum Response {
    Hello(HelloResponse),
    TaskCreate(TaskCreateResponse),
    TaskStatus(TaskStatusResponse),
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
        });
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
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
