//! `EmbeddedClient`: the Phase 3 implementation of `valyria_protocol::
//! Client` — the runtime linked in-process, per D11's default. A future
//! daemon transport (Phase 10) implements the same trait against a socket;
//! no `valyria-cli` call site changes when that lands.

use std::sync::Arc;

use futures::stream::{BoxStream, StreamExt};
use valyria_events::{Delivery, EventEnvelope, Seq};
use valyria_protocol::{
    Client, HelloResponse, PermissionResolveRequest, Request, Response, TaskCreateResponse,
    TaskIdRequest, TaskStatusResponse, WireError, WireEvent, PROTOCOL_VERSION,
};
use valyria_types::{ErrorCode, TaskId};

use crate::runtime::Runtime;

pub struct EmbeddedClient {
    runtime: Arc<Runtime>,
}

impl EmbeddedClient {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }
}

fn parse_task_id(raw: &str) -> Result<TaskId, Response> {
    raw.parse().map_err(|_| {
        error_response_raw(
            "app.invalid_task_id",
            format!("not a valid task id: {raw}"),
            false,
        )
    })
}

fn error_response<E: ErrorCode + std::fmt::Display>(err: E) -> Response {
    error_response_raw(err.code(), err.to_string(), err.retryable())
}

fn error_response_raw(code: &str, message: String, retryable: bool) -> Response {
    Response::Error(WireError {
        code: code.to_string(),
        message,
        retryable,
    })
}

fn task_id_from(req: TaskIdRequest) -> Result<TaskId, Response> {
    parse_task_id(&req.task_id)
}

#[async_trait::async_trait]
impl Client for EmbeddedClient {
    async fn call(&self, req: Request) -> Response {
        match req {
            Request::Hello(_) => Response::Hello(HelloResponse {
                protocol_version: PROTOCOL_VERSION.to_string(),
                runtime_version: env!("CARGO_PKG_VERSION").to_string(),
            }),
            Request::TaskCreate(r) => match self.runtime.create_and_start_task(r.objective).await {
                Ok(task_id) => Response::TaskCreate(TaskCreateResponse {
                    task_id: task_id.to_string(),
                }),
                Err(e) => error_response(e),
            },
            Request::TaskStatus(r) => {
                let task_id = match parse_task_id(&r.task_id) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                match self.runtime.task_status(task_id).await {
                    Ok(task) => Response::TaskStatus(TaskStatusResponse {
                        task_id: task.id.to_string(),
                        objective: task.objective,
                        state: task.state.to_string(),
                        paused_from: task.paused_from.map(|s| s.to_string()),
                        recovery_note: task.recovery_note,
                    }),
                    Err(e) => error_response(e),
                }
            }
            Request::TaskPause(r) => {
                let task_id = match task_id_from(r) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                match self.runtime.pause_task(task_id).await {
                    Ok(()) => Response::Ack,
                    Err(e) => error_response(e),
                }
            }
            Request::TaskResume(r) => {
                let task_id = match task_id_from(r) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                match self.runtime.resume_task(task_id).await {
                    Ok(()) => Response::Ack,
                    Err(e) => error_response(e),
                }
            }
            Request::TaskCancel(r) => {
                let task_id = match task_id_from(r) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                match self.runtime.cancel_task(task_id).await {
                    Ok(()) => Response::Ack,
                    Err(e) => error_response(e),
                }
            }
            Request::PermissionResolve(PermissionResolveRequest { task_id, approve }) => {
                let task_id = match parse_task_id(&task_id) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                match self.runtime.resolve_permission(task_id, approve).await {
                    Ok(()) => Response::Ack,
                    Err(e) => error_response(e),
                }
            }
            Request::EventsSubscribe(_) => {
                // Handled by `subscribe_events`, not `call` — a client that
                // routes this through `call` gets a clear error rather than
                // a silent no-op.
                error_response_raw(
                    "protocol.use_subscribe_events",
                    "events.subscribe must be issued via Client::subscribe_events, not call"
                        .to_string(),
                    false,
                )
            }
        }
    }

    async fn subscribe_events(&self, since: u64) -> BoxStream<'static, WireEvent> {
        let events = self.runtime.events();
        let initial = events
            .subscribe_since(Seq(since))
            .await
            .expect("subscribing to a live EventBus does not fail");

        futures::stream::unfold((events, initial), |(events, mut sub)| async move {
            loop {
                match sub.recv().await {
                    Ok(Delivery::Event(env)) => return Some((to_wire_event(env), (events, sub))),
                    Ok(Delivery::Lagged { resume_from }) => {
                        match events.subscribe_since(resume_from).await {
                            Ok(fresh) => sub = fresh,
                            Err(_) => return None,
                        }
                    }
                    Err(_) => return None,
                }
            }
        })
        .boxed()
    }
}

fn to_wire_event(env: EventEnvelope) -> WireEvent {
    WireEvent {
        seq: env.seq.0,
        task_id: env.task_id.map(|t| t.to_string()),
        ts_ms: env.ts.as_millis(),
        kind: env.kind.as_str().to_string(),
        payload: env.payload,
    }
}
