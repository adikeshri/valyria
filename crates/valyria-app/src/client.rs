//! `EmbeddedClient`: the in-process implementation of `valyria_protocol::
//! Client` — the runtime linked directly, per D11's default. The
//! Unix-socket daemon ([`crate::daemon::serve`]) wraps *this same type*,
//! so no `valyria-cli` call site changes between the two transports.

use std::sync::Arc;

use futures::stream::{BoxStream, StreamExt};
use valyria_events::{Delivery, EventEnvelope, Seq};
use valyria_protocol::{
    capability, Client, ConfigEntryWire, ConfigShowResponse, DoctorCheckWire, DoctorRunResponse,
    GitBranchWire, GitBranchesResponse, GitCommitWire, GitDiffResponse, GitFileStatusWire,
    GitLogResponse, GitStatusResponse, HelloResponse, IndexStatusResponse, MemoryEntryWire,
    MemoryListRequest, MemoryListResponse, ModelListResponse, ModelSummaryWire,
    PermissionResolveRequest, PlanGetResponse, PlanStepSummary, PurgeResponse, Request, Response,
    ScoreExplanationWire, SearchFeatureWire, SearchHitWire, SearchQueryResponse,
    SearchStageScoreWire, StorageEntryWire, StorageInspectResponse, StoragePurgeRequest,
    TaskCreateResponse, TaskIdRequest, TaskListResponse, TaskReportResponse, TaskRollbackRequest,
    TaskRollbackResponse, TaskStatusResponse, TaskSummary, VerifiedClaimWire, WireError, WireEvent,
    WorkspaceStatusResponse, PROTOCOL_VERSION,
};
use valyria_types::{CheckpointId, ErrorCode, PermissionMode, TaskId};

use crate::doctor::CheckStatus;
use crate::runtime::Runtime;
use crate::storage::PurgeScope;

pub struct EmbeddedClient {
    runtime: Arc<Runtime>,
}

impl EmbeddedClient {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }
}

fn parse_permission_mode(raw: &str) -> Result<PermissionMode, Response> {
    match raw {
        "manual" => Ok(PermissionMode::Manual),
        "assisted" => Ok(PermissionMode::Assisted),
        "autonomous" => Ok(PermissionMode::Autonomous),
        other => Err(error_response_raw(
            "protocol.invalid_permission_mode",
            format!("unknown permission_mode `{other}` (expected: manual, assisted, autonomous)"),
            false,
        )),
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

fn task_summary(t: &valyria_task::Task) -> TaskSummary {
    TaskSummary {
        task_id: t.id.to_string(),
        objective: t.objective.clone(),
        state: t.state.to_string(),
        created_at_ms: t.created_at.as_millis() as u64,
        updated_at_ms: t.updated_at.as_millis() as u64,
    }
}

fn git_file_status_wire(f: &valyria_git::FileStatus) -> GitFileStatusWire {
    use valyria_git::StatusKind;
    let kind = match f.kind {
        StatusKind::Added => "added",
        StatusKind::Modified => "modified",
        StatusKind::Deleted => "deleted",
        StatusKind::Untracked => "untracked",
        StatusKind::Conflicted => "conflicted",
    };
    GitFileStatusWire {
        path: f.path.clone(),
        kind: kind.to_string(),
        staged: f.staged,
    }
}

fn search_results_wire(r: valyria_search::SearchResults) -> SearchQueryResponse {
    SearchQueryResponse {
        hits: r.hits.into_iter().map(search_hit_wire).collect(),
        modes_run: r.modes_run.iter().map(|m| m.as_str().to_string()).collect(),
        degraded: r.degraded,
    }
}

fn search_hit_wire(h: valyria_search::SearchHit) -> SearchHitWire {
    SearchHitWire {
        path: h.path,
        symbol_path: h.symbol_path,
        line: h.line,
        snippet: h.snippet,
        score: h.score,
        explanation: ScoreExplanationWire {
            stage_scores: h
                .explanation
                .stage_scores
                .into_iter()
                .map(|s| SearchStageScoreWire {
                    mode: s.mode.as_str().to_string(),
                    rank: s.rank as u32,
                    raw_score: s.raw_score,
                })
                .collect(),
            features: h
                .explanation
                .features
                .into_iter()
                .map(|f| SearchFeatureWire {
                    name: f.name,
                    value: f.value,
                    weight: f.weight,
                    contribution: f.contribution,
                })
                .collect(),
            retrieval_paths: h.explanation.retrieval_paths,
        },
    }
}

#[async_trait::async_trait]
impl Client for EmbeddedClient {
    async fn call(&self, req: Request) -> Response {
        match req {
            Request::Hello(_) => Response::Hello(HelloResponse {
                protocol_version: PROTOCOL_VERSION.to_string(),
                runtime_version: env!("CARGO_PKG_VERSION").to_string(),
                capabilities: capability::ALL.iter().map(|s| s.to_string()).collect(),
            }),
            Request::TaskCreate(r) => {
                let mode = match r.permission_mode.as_deref().map(parse_permission_mode) {
                    Some(Ok(m)) => Some(m),
                    Some(Err(resp)) => return resp,
                    None => None,
                };
                match self
                    .runtime
                    .create_and_start_task_with_mode(r.objective, mode)
                    .await
                {
                    Ok(task_id) => Response::TaskCreate(TaskCreateResponse {
                        task_id: task_id.to_string(),
                    }),
                    Err(e) => error_response(e),
                }
            }
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
            Request::TaskList(_) => match self.runtime.list_tasks().await {
                Ok(tasks) => Response::TaskList(TaskListResponse {
                    tasks: tasks.iter().map(task_summary).collect(),
                }),
                Err(e) => error_response(e),
            },
            Request::TaskReport(r) => {
                let task_id = match task_id_from(r) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                match self.runtime.completion_report(task_id).await {
                    Ok(report) => Response::TaskReport(TaskReportResponse {
                        task_id: report.task_id.to_string(),
                        status: serde_json::to_value(report.status)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_else(|| "not_verified".to_string()),
                        verified: report
                            .verified
                            .into_iter()
                            .map(|v| VerifiedClaimWire {
                                kind: v.kind,
                                command: v.command,
                                outcome: v.outcome,
                                run_id: v.run_id,
                            })
                            .collect(),
                        unverified: report.unverified,
                    }),
                    Err(e) => error_response(e),
                }
            }
            Request::TaskPlan(r) => {
                let task_id = match task_id_from(r) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                match self.runtime.plan(task_id).await {
                    Ok(Some(rev)) => Response::TaskPlan(PlanGetResponse {
                        revision: Some(rev.revision),
                        content_hash: Some(rev.plan.content_hash().to_hex()),
                        steps: rev
                            .plan
                            .steps
                            .iter()
                            .map(|s| PlanStepSummary {
                                id: s.id.to_string(),
                                intent: s.intent.clone(),
                                targets: s
                                    .targets
                                    .iter()
                                    .map(|p| p.display().to_string())
                                    .collect(),
                                depends_on: s.depends_on.iter().map(|d| d.to_string()).collect(),
                                rollback_boundary: s.rollback_boundary,
                                checkpoint: s.checkpoint,
                            })
                            .collect(),
                    }),
                    Ok(None) => Response::TaskPlan(PlanGetResponse {
                        revision: None,
                        content_hash: None,
                        steps: vec![],
                    }),
                    Err(e) => error_response(e),
                }
            }
            Request::TaskRollback(TaskRollbackRequest {
                task_id,
                checkpoint_id,
            }) => {
                let task_id = match parse_task_id(&task_id) {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                let checkpoint_id = match checkpoint_id.parse::<CheckpointId>() {
                    Ok(id) => id,
                    Err(_) => {
                        return error_response_raw(
                            "app.invalid_checkpoint_id",
                            format!("not a valid checkpoint id: {checkpoint_id}"),
                            false,
                        )
                    }
                };
                match self
                    .runtime
                    .rollback_to_checkpoint(task_id, checkpoint_id)
                    .await
                {
                    Ok(report) => Response::TaskRollback(TaskRollbackResponse {
                        reverted_entries: report.reverted.len() as u64,
                        restored_files: report
                            .reverted
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect(),
                    }),
                    Err(e) => error_response_raw("app.rollback", e.to_string(), false),
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
            Request::WorkspaceStatus(_) => {
                let index_generation = self
                    .runtime
                    .current_index_generation()
                    .await
                    .unwrap_or(None);
                let tasks = self.runtime.list_tasks().await.unwrap_or_default();
                let active = tasks.iter().filter(|t| !t.state.is_terminal()).count() as u32;
                Response::WorkspaceStatus(WorkspaceStatusResponse {
                    workspace_id: self.runtime.workspace_id().to_string(),
                    root: self.runtime.workspace_path().display().to_string(),
                    data_dir: self.runtime.data_dir().display().to_string(),
                    index_generation,
                    active_tasks: active,
                    total_tasks: tasks.len() as u32,
                })
            }
            Request::DoctorRun(_) => {
                let report = self.runtime.doctor().await;
                let summary = status_str(report.summary());
                Response::DoctorRun(DoctorRunResponse {
                    checks: report
                        .checks
                        .into_iter()
                        .map(|c| DoctorCheckWire {
                            name: c.name,
                            status: status_str(c.status).to_string(),
                            detail: c.detail,
                            remediation: c.remediation,
                        })
                        .collect(),
                    summary: summary.to_string(),
                })
            }
            Request::StorageInspect(_) => {
                let report = self.runtime.storage_inspect();
                Response::StorageInspect(StorageInspectResponse {
                    total_bytes: report.total_bytes(),
                    entries: report
                        .entries
                        .into_iter()
                        .map(|e| StorageEntryWire {
                            name: e.name,
                            bytes: e.bytes,
                            detail: e.detail,
                            purgeable: e.purgeable,
                        })
                        .collect(),
                })
            }
            Request::StoragePurge(StoragePurgeRequest { scope, dry_run }) => {
                let Some(scope) = PurgeScope::parse(&scope) else {
                    return error_response_raw(
                        "app.unknown_purge_scope",
                        format!(
                            "unknown purge scope `{scope}` (expected: memory, cache, tasks, logs)"
                        ),
                        false,
                    );
                };
                match self.runtime.storage_purge(scope, dry_run).await {
                    Ok(out) => Response::Purge(PurgeResponse {
                        freed_bytes: out.freed_bytes,
                        items_removed: out.items_removed,
                        dry_run: out.dry_run,
                    }),
                    Err(e) => error_response(e),
                }
            }
            Request::ConfigShow(_) => match self.runtime.config_show() {
                Ok(entries) => Response::ConfigShow(ConfigShowResponse {
                    entries: entries
                        .into_iter()
                        .map(|(key, value, origin)| ConfigEntryWire { key, value, origin })
                        .collect(),
                }),
                Err(e) => error_response(e),
            },
            Request::ConfigSet(r) => {
                let Some(scope) = crate::runtime::ConfigWriteScope::parse(&r.scope) else {
                    return error_response_raw(
                        "config.invalid_scope",
                        format!(
                            "unknown config scope `{}` (expected: workspace, user)",
                            r.scope
                        ),
                        false,
                    );
                };
                match self.runtime.config_set(&r.key, &r.value, scope) {
                    Ok(entries) => Response::ConfigShow(ConfigShowResponse {
                        entries: entries
                            .into_iter()
                            .map(|(key, value, origin)| ConfigEntryWire { key, value, origin })
                            .collect(),
                    }),
                    Err(e) => error_response(e),
                }
            }
            Request::MemoryList(MemoryListRequest { query, limit }) => {
                let limit = limit.unwrap_or(20).min(200) as usize;
                match self.runtime.memory_list(query.as_deref(), limit).await {
                    Ok(entries) => Response::MemoryList(MemoryListResponse {
                        entries: entries
                            .into_iter()
                            .map(|e| {
                                let ec = e.effective_confidence(
                                    chrono_now_ms(),
                                    valyria_memory::DEFAULT_HALF_LIFE_MS,
                                );
                                MemoryEntryWire {
                                    id: e.id.to_string(),
                                    kind: format!("{:?}", e.kind).to_lowercase(),
                                    scope: memory_scope_str(&e.scope).to_string(),
                                    author: format!("{:?}", e.author).to_lowercase(),
                                    text: e.text,
                                    effective_confidence: ec,
                                }
                            })
                            .collect(),
                    }),
                    Err(e) => error_response(e),
                }
            }
            Request::ModelList(_) => match self.runtime.model_list().await {
                Ok(pairs) => Response::ModelList(ModelListResponse {
                    models: pairs
                        .into_iter()
                        .map(|(c, installed)| ModelSummaryWire {
                            id: c.id,
                            family: c.family,
                            quantization: c.quantization.as_str().to_string(),
                            size_bytes: c.file_size_bytes,
                            installed,
                            license: c.license_name,
                        })
                        .collect(),
                }),
                Err(e) => error_response(e),
            },
            Request::GitStatus(_) => match self.runtime.git_status() {
                Ok(v) => Response::GitStatus(GitStatusResponse {
                    branch: v.branch,
                    detached: v.detached,
                    head_commit: v.head_commit,
                    files: v.files.iter().map(git_file_status_wire).collect(),
                }),
                Err(e) => error_response(e),
            },
            Request::GitDiff(r) => match self.runtime.git_diff(r.path.as_deref(), r.staged) {
                Ok(d) => Response::GitDiff(GitDiffResponse {
                    unified: d.unified,
                    truncated: d.truncated,
                }),
                Err(e) => error_response(e),
            },
            Request::GitLog(r) => {
                let limit = r.limit.unwrap_or(50) as usize;
                match self.runtime.git_log(limit) {
                    Ok(commits) => Response::GitLog(GitLogResponse {
                        commits: commits
                            .into_iter()
                            .map(|c| GitCommitWire {
                                sha: c.sha,
                                author_name: c.author_name,
                                author_email: c.author_email,
                                message: c.message,
                                time_unix: c.time,
                                parents: c.parents,
                            })
                            .collect(),
                    }),
                    Err(e) => error_response(e),
                }
            }
            Request::GitBranches(_) => match self.runtime.git_branches() {
                Ok(branches) => Response::GitBranches(GitBranchesResponse {
                    branches: branches
                        .into_iter()
                        .map(|b| GitBranchWire {
                            name: b.name,
                            commit: b.commit,
                            is_head: b.is_head,
                        })
                        .collect(),
                }),
                Err(e) => error_response(e),
            },
            Request::SearchQuery(r) => {
                let mut modes = Vec::with_capacity(r.modes.len());
                for m in &r.modes {
                    match valyria_search::SearchMode::parse(m) {
                        Some(mode) => modes.push(mode),
                        None => {
                            return error_response_raw(
                                "search.unknown_mode",
                                format!(
                                    "unknown search mode `{m}` (expected: lexical, regex, symbol, \
                                     semantic, ast, dependency, git)"
                                ),
                                false,
                            )
                        }
                    }
                }
                let mut query = valyria_search::SearchQuery::new(r.query);
                query.modes = modes;
                query.anchors = r.anchors;
                query.limit = r.limit.unwrap_or(20).min(200) as usize;
                match self.runtime.search(&query) {
                    Ok(results) => Response::SearchQuery(search_results_wire(results)),
                    Err(e) => error_response(e),
                }
            }
            Request::IndexStatus(_) => match self.runtime.index_status().await {
                Ok(Some(g)) => Response::IndexStatus(IndexStatusResponse {
                    generation: Some(g.generation.0),
                    stage: Some(format!("{:?}", g.stage).to_lowercase()),
                    file_count: g.file_count,
                    symbol_count: g.symbol_count,
                    created_at_ms: Some(g.created_at_ms),
                }),
                Ok(None) => Response::IndexStatus(IndexStatusResponse {
                    generation: None,
                    stage: None,
                    file_count: 0,
                    symbol_count: 0,
                    created_at_ms: None,
                }),
                Err(e) => error_response(e),
            },
            Request::EventsSubscribe(_) => error_response_raw(
                "protocol.use_subscribe_events",
                "events.subscribe must be issued via Client::subscribe_events, not call"
                    .to_string(),
                false,
            ),
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

fn status_str(s: CheckStatus) -> &'static str {
    s.as_str()
}

fn memory_scope_str(scope: &valyria_memory::MemoryScope) -> &'static str {
    use valyria_memory::MemoryScope;
    match scope {
        MemoryScope::Session(_) => "session",
        MemoryScope::Task(_) => "task",
        MemoryScope::Repository => "repository",
        MemoryScope::User => "user",
    }
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
