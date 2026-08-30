//! `valyria` — the CLI. A thin protocol client (layer 6, D11): every
//! command goes through `valyria_protocol::Client`, either an embedded
//! `valyria_app::Runtime` (default) or a `SocketClient` against a running
//! daemon (`--connect <socket>`). This binary contains no state-machine,
//! journal, or tool-invocation logic of its own; its `Cargo.toml` lists
//! only `valyria-app` / `valyria-protocol` / `valyria-types` /
//! `valyria-util` from the runtime, plus a terminal UI toolkit for the
//! interactive session.

mod args;
mod render;
mod tui;

use std::process::ExitCode;
use std::sync::Arc;

use futures::StreamExt;
use valyria_app::{load_scenario, serve, EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{
    Client, Empty, MemoryListRequest, PermissionResolveRequest, Request, Response,
    StoragePurgeRequest, TaskCreateRequest, TaskIdRequest, TaskRollbackRequest, TaskStatusRequest,
};
use valyria_util::CancellationToken;

use args::{parse, resolve_workspace, ParsedArgs};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("valyria {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") | Some("help") => {
            print_usage();
            ExitCode::SUCCESS
        }
        None => tokio_main(cmd_tui(vec![])),
        Some("tui") => tokio_main(cmd_tui(argv[1..].to_vec())),
        Some("run") => tokio_main(cmd_run(argv[1..].to_vec())),
        Some("task") => tokio_main(cmd_task(argv[1..].to_vec())),
        Some("doctor") => tokio_main(cmd_doctor(argv[1..].to_vec())),
        Some("clean") => tokio_main(cmd_clean(argv[1..].to_vec())),
        Some("status") => tokio_main(cmd_status(argv[1..].to_vec())),
        Some("config") => tokio_main(cmd_config(argv[1..].to_vec())),
        Some("model") => tokio_main(cmd_model(argv[1..].to_vec())),
        Some("memory") => tokio_main(cmd_memory(argv[1..].to_vec())),
        Some("serve") => tokio_main(cmd_serve(argv[1..].to_vec())),
        _ => {
            print_usage();
            ExitCode::from(64) // EX_USAGE
        }
    }
}

fn tokio_main(fut: impl std::future::Future<Output = ExitCode>) -> ExitCode {
    tokio::runtime::Runtime::new()
        .expect("failed to start the tokio runtime")
        .block_on(fut)
}

fn print_usage() {
    eprintln!(
        "valyria {} — a thin protocol client (D11)",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    valyria                                 open the interactive session (TUI)");
    eprintln!("    valyria run \"<objective>\" [flags]        start and watch a task");
    eprintln!("    valyria task <status|list|report|plan|rollback|pause|resume|cancel|permission>");
    eprintln!("    valyria doctor [--json]                 diagnose the environment");
    eprintln!("    valyria status [--json]                 workspace / index / task summary");
    eprintln!(
        "    valyria config [--json]                 effective config + where each value came from"
    );
    eprintln!("    valyria model list [--json]             catalog, with install state");
    eprintln!("    valyria memory list [<query>] [--json]  inspect stored memory");
    eprintln!("    valyria clean --scope <memory|cache|tasks|logs> [--dry-run] [--json]");
    eprintln!("    valyria serve [--socket <path>]         run the daemon");
    eprintln!();
    eprintln!("COMMON FLAGS:");
    eprintln!("    --workspace <path>     workspace root (default: cwd)");
    eprintln!("    --connect <socket>     talk to a running daemon instead of an embedded runtime");
    eprintln!("    --json                 machine-readable output");
    eprintln!("    --events               (run) also print the raw event stream");
    eprintln!("    --scenario <file.toml> (run) drive with a fake-model scenario");
    eprintln!("    --permission-mode <manual|assisted|autonomous>");
    eprintln!("    --plan                 (run) model-authored, validated plan (Phase 8)");
}

/// A `Client` plus whatever must stay alive for its lifetime (the embedded
/// `Runtime`, held inside the `Arc<EmbeddedClient>`). `--connect` swaps in
/// a `SocketClient` and nothing else changes.
async fn build_client(parsed: &ParsedArgs) -> Result<Arc<dyn Client>, String> {
    if let Some(socket) = &parsed.connect {
        let client = match &parsed.auth_token_file {
            Some(path) => {
                let token = std::fs::read_to_string(path)
                    .map_err(|e| {
                        format!("failed to read --auth-token-file {}: {e}", path.display())
                    })?
                    .trim()
                    .to_string();
                valyria_protocol::SocketClient::with_token(socket, token)
            }
            None => valyria_protocol::SocketClient::new(socket),
        };
        return Ok(Arc::new(client));
    }
    let workspace_path = resolve_workspace(parsed);
    let mut config = RuntimeConfig::new(workspace_path);
    if let Some(mode) = parsed.permission_mode {
        config = config.with_permission_mode(mode);
    }
    if let Some(path) = &parsed.scenario {
        let scenario = load_scenario(path).map_err(|e| format!("failed to load scenario: {e}"))?;
        config = config.with_scenario(scenario);
    }
    if parsed.plan {
        config = config.with_planning_mode(valyria_app::PlanningMode::ModelAuthored);
    }
    let runtime = Runtime::open(config)
        .await
        .map_err(|e| format!("failed to open runtime: {e}"))?;
    Ok(Arc::new(EmbeddedClient::new(Arc::new(runtime))))
}

fn print_error_and_fail(context: &str, message: &str) -> ExitCode {
    eprintln!("error: {context}: {message}");
    ExitCode::FAILURE
}

/// Shared helper: parse args, build a client, issue one request, render it.
/// Used by every read-only command (`doctor`, `status`, `config`, ...).
async fn one_shot(
    raw: Vec<String>,
    context: &'static str,
    make_request: impl FnOnce(&ParsedArgs) -> Request,
    render_human: impl FnOnce(&Response),
) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail(context, &e),
    };
    let response = client.call(make_request(&parsed)).await;
    if let Response::Error(e) = &response {
        return print_error_and_fail(context, &e.message);
    }
    if parsed.json {
        println!("{}", render::to_json(&response));
    } else {
        render_human(&response);
    }
    ExitCode::SUCCESS
}

async fn cmd_doctor(raw: Vec<String>) -> ExitCode {
    one_shot(
        raw,
        "doctor",
        |_| Request::DoctorRun(Empty {}),
        render::doctor,
    )
    .await
}

async fn cmd_status(raw: Vec<String>) -> ExitCode {
    one_shot(
        raw,
        "status",
        |_| Request::WorkspaceStatus(Empty {}),
        render::workspace_status,
    )
    .await
}

async fn cmd_config(raw: Vec<String>) -> ExitCode {
    one_shot(
        raw,
        "config",
        |_| Request::ConfigShow(Empty {}),
        render::config_show,
    )
    .await
}

async fn cmd_model(raw: Vec<String>) -> ExitCode {
    // Only subcommand today: `model list`.
    let rest: Vec<String> = raw
        .iter()
        .filter(|a| a.as_str() != "list")
        .cloned()
        .collect();
    one_shot(
        rest,
        "model list",
        |_| Request::ModelList(Empty {}),
        render::model_list,
    )
    .await
}

async fn cmd_memory(raw: Vec<String>) -> ExitCode {
    if raw.first().map(String::as_str) != Some("list") {
        eprintln!("error: usage: valyria memory list [<query>] [--json]");
        return ExitCode::from(64);
    }
    one_shot(
        raw[1..].to_vec(),
        "memory list",
        |p| {
            Request::MemoryList(MemoryListRequest {
                query: p.positional.first().cloned(),
                limit: Some(20),
            })
        },
        render::memory_list,
    )
    .await
}

async fn cmd_clean(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let Some(scope) = parsed.scope.clone() else {
        eprintln!("error: `valyria clean` needs --scope <memory|cache|tasks|logs>");
        return ExitCode::from(64);
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("clean", &e),
    };
    let response = client
        .call(Request::StoragePurge(StoragePurgeRequest {
            scope,
            dry_run: parsed.dry_run,
        }))
        .await;
    match &response {
        Response::Error(e) => print_error_and_fail("clean", &e.message),
        _ if parsed.json => {
            println!("{}", render::to_json(&response));
            ExitCode::SUCCESS
        }
        _ => {
            render::purge(&response);
            ExitCode::SUCCESS
        }
    }
}

async fn cmd_serve(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    if parsed.connect.is_some() {
        return print_error_and_fail("serve", "--connect makes no sense for `serve`");
    }
    let workspace_path = resolve_workspace(&parsed);
    let mut config = RuntimeConfig::new(workspace_path);
    if let Some(mode) = parsed.permission_mode {
        config = config.with_permission_mode(mode);
    }
    if parsed.plan {
        config = config.with_planning_mode(valyria_app::PlanningMode::ModelAuthored);
    }
    let runtime = match Runtime::open(config).await {
        Ok(r) => Arc::new(r),
        Err(e) => return print_error_and_fail("serve", &format!("failed to open runtime: {e}")),
    };
    let socket = parsed
        .socket
        .clone()
        .unwrap_or_else(|| runtime.data_dir().join("valyria.sock"));
    println!(
        "valyria daemon: workspace {}",
        runtime.workspace_path().display()
    );
    println!("listening on {}", socket.display());
    println!("stop with Ctrl-C");

    let shutdown = CancellationToken::new();
    let sig = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nshutting down…");
        sig.cancel();
    });

    let auth_token = match &parsed.auth_token_file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(t) => Some(t.trim().to_string()),
            Err(e) => {
                return print_error_and_fail(
                    "serve",
                    &format!("failed to read --auth-token-file {}: {e}", path.display()),
                )
            }
        },
        None => None,
    };

    match serve(runtime, &socket, shutdown, auth_token).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => print_error_and_fail("serve", &e.to_string()),
    }
}

async fn cmd_tui(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("tui", &e),
    };
    match tui::run(client).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => print_error_and_fail("tui", &e.to_string()),
    }
}

async fn cmd_run(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let Some(objective) = parsed.positional.first().cloned() else {
        eprintln!("error: `valyria run` needs an objective, e.g. `valyria run \"add a function\"`");
        return ExitCode::from(64);
    };

    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("run", &e),
    };

    let response = client
        .call(Request::TaskCreate(TaskCreateRequest {
            objective,
            permission_mode: None,
        }))
        .await;
    let task_id = match response {
        Response::TaskCreate(r) => r.task_id,
        Response::Error(e) => return print_error_and_fail("task create", &e.message),
        other => {
            return print_error_and_fail("task create", &format!("unexpected response: {other:?}"))
        }
    };
    println!("task_id: {task_id}");
    // Explicit flush: stdout is block-buffered once piped rather than
    // attached to a terminal, so without this a reader waiting on this
    // line — including a test that kills this process immediately
    // afterward to exercise crash recovery — could otherwise never see it.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    watch_task_to_terminal(client.as_ref(), &task_id, parsed.events).await
}

/// Streams events from `since: 0` and watches this task's `state_changed`
/// until it reaches a state worth stopping at. Safe to start from `since:
/// 0` here specifically because task creation is this task's very first
/// event — contrast `watch_established_task_to_terminal`.
async fn watch_task_to_terminal(
    client: &dyn Client,
    task_id: &str,
    print_events: bool,
) -> ExitCode {
    let events = client.subscribe_events(0).await;
    watch_stream_to_terminal(client, events, task_id, print_events).await
}

/// For a task that may already have history *before* the transition we
/// care about (a prior `WAITING_FOR_PERMISSION` from an earlier
/// invocation): subscribe, drain the already-backlogged events (bounded by
/// a short per-read timeout), *then* issue `after_subscribing`, so
/// everything read afterward is guaranteed new.
async fn watch_established_task_to_terminal<F>(
    client: &dyn Client,
    task_id: &str,
    print_events: bool,
    after_subscribing: F,
) -> ExitCode
where
    F: std::future::Future<Output = Result<(), ExitCode>>,
{
    let mut events = client.subscribe_events(0).await;
    while tokio::time::timeout(std::time::Duration::from_millis(50), events.next())
        .await
        .is_ok()
    {}

    if let Err(code) = after_subscribing.await {
        return code;
    }

    watch_stream_to_terminal(client, events, task_id, print_events).await
}

async fn watch_stream_to_terminal(
    client: &dyn Client,
    mut events: futures::stream::BoxStream<'static, valyria_protocol::WireEvent>,
    task_id: &str,
    print_events: bool,
) -> ExitCode {
    loop {
        let Some(event) = events.next().await else {
            eprintln!("error: event stream ended unexpectedly");
            return ExitCode::FAILURE;
        };
        if print_events {
            println!("{}", serde_json::to_string(&event).unwrap());
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        if event.task_id.as_deref() != Some(task_id) {
            continue;
        }
        if event.kind != "state_changed" {
            continue;
        }
        let Some(to) = event.payload.get("to").and_then(|v| v.as_str()) else {
            continue;
        };
        match to {
            "COMPLETED" => {
                println!("task {task_id} completed");
                return ExitCode::SUCCESS;
            }
            "FAILED" => {
                println!("task {task_id} failed");
                return ExitCode::from(1);
            }
            "CANCELLED" => {
                println!("task {task_id} cancelled");
                return ExitCode::from(2);
            }
            "WAITING_FOR_PERMISSION" => {
                println!(
                    "task {task_id} is waiting for a permission decision — resolve with \
                     `valyria task permission resolve {task_id} --allow|--deny`"
                );
                return ExitCode::from(3);
            }
            "WAITING_FOR_USER" => {
                println!(
                    "task {task_id} is waiting for user input (not yet actionable from the CLI)"
                );
                return ExitCode::from(3);
            }
            "PAUSED" => {
                // `PAUSED` can be transient: crash-recovery legitimately
                // passes a task through `Paused` for a moment. Confirm
                // against the *current* live status before treating it as
                // the task actually being at rest.
                match client
                    .call(Request::TaskStatus(TaskStatusRequest {
                        task_id: task_id.to_string(),
                    }))
                    .await
                {
                    Response::TaskStatus(status) if status.state == "PAUSED" => {
                        println!(
                            "task {task_id} paused — resume with `valyria task resume {task_id}`"
                        );
                        return ExitCode::from(4);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

async fn cmd_task(raw: Vec<String>) -> ExitCode {
    if raw.is_empty() {
        print_usage();
        return ExitCode::from(64);
    }
    match raw[0].as_str() {
        "status" => cmd_task_status(raw[1..].to_vec()).await,
        "list" => {
            one_shot(
                raw[1..].to_vec(),
                "task list",
                |_| Request::TaskList(Empty {}),
                render::task_list,
            )
            .await
        }
        "report" => {
            cmd_task_one_shot(
                raw[1..].to_vec(),
                "task report",
                |id| Request::TaskReport(TaskIdRequest { task_id: id }),
                render::task_report,
            )
            .await
        }
        "plan" => {
            cmd_task_one_shot(
                raw[1..].to_vec(),
                "task plan",
                |id| Request::TaskPlan(TaskIdRequest { task_id: id }),
                render::task_plan,
            )
            .await
        }
        "rollback" => cmd_task_rollback(raw[1..].to_vec()).await,
        "pause" => {
            cmd_task_signal(raw[1..].to_vec(), |id| {
                Request::TaskPause(TaskIdRequest { task_id: id })
            })
            .await
        }
        "resume" => cmd_task_resume(raw[1..].to_vec()).await,
        "cancel" => {
            cmd_task_signal(raw[1..].to_vec(), |id| {
                Request::TaskCancel(TaskIdRequest { task_id: id })
            })
            .await
        }
        "permission" if raw.get(1).map(String::as_str) == Some("resolve") => {
            cmd_permission_resolve(raw[2..].to_vec()).await
        }
        other => {
            eprintln!("error: unknown `valyria task` subcommand `{other}`");
            print_usage();
            ExitCode::from(64)
        }
    }
}

/// `valyria task <verb> <task_id> [--json]` for the read-only verbs.
async fn cmd_task_one_shot(
    raw: Vec<String>,
    context: &'static str,
    build_request: impl FnOnce(String) -> Request,
    render_human: impl FnOnce(&Response),
) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let Some(task_id) = parsed.positional.first().cloned() else {
        eprintln!("error: {context} needs a task id");
        return ExitCode::from(64);
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail(context, &e),
    };
    let response = client.call(build_request(task_id)).await;
    if let Response::Error(e) = &response {
        return print_error_and_fail(context, &e.message);
    }
    if parsed.json {
        println!("{}", render::to_json(&response));
    } else {
        render_human(&response);
    }
    ExitCode::SUCCESS
}

async fn cmd_task_rollback(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let (Some(task_id), Some(checkpoint_id)) = (
        parsed.positional.first().cloned(),
        parsed.positional.get(1).cloned(),
    ) else {
        eprintln!("error: usage: valyria task rollback <task_id> <checkpoint_id>");
        return ExitCode::from(64);
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("task rollback", &e),
    };
    let response = client
        .call(Request::TaskRollback(TaskRollbackRequest {
            task_id,
            checkpoint_id,
        }))
        .await;
    match &response {
        Response::Error(e) => print_error_and_fail("task rollback", &e.message),
        _ if parsed.json => {
            println!("{}", render::to_json(&response));
            ExitCode::SUCCESS
        }
        Response::TaskRollback(r) => {
            println!("rolled back {} file(s):", r.reverted_entries);
            for f in &r.restored_files {
                println!("  {f}");
            }
            ExitCode::SUCCESS
        }
        other => print_error_and_fail("task rollback", &format!("unexpected response: {other:?}")),
    }
}

async fn cmd_task_status(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let Some(task_id) = parsed.positional.first().cloned() else {
        eprintln!("error: `valyria task status` needs a task id");
        return ExitCode::from(64);
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("task status", &e),
    };
    let response = client
        .call(Request::TaskStatus(TaskStatusRequest { task_id }))
        .await;
    match &response {
        Response::TaskStatus(status) if parsed.json => {
            let _ = status;
            println!("{}", render::to_json(&response));
            ExitCode::SUCCESS
        }
        Response::TaskStatus(status) => {
            println!("task_id: {}", status.task_id);
            println!("objective: {}", status.objective);
            println!("state: {}", status.state);
            if let Some(paused_from) = &status.paused_from {
                println!("paused_from: {paused_from}");
            }
            if let Some(note) = &status.recovery_note {
                println!("recovery_note: {note}");
            }
            ExitCode::SUCCESS
        }
        Response::Error(e) => print_error_and_fail("task status", &e.message),
        other => print_error_and_fail("task status", &format!("unexpected response: {other:?}")),
    }
}

async fn cmd_task_signal(
    raw: Vec<String>,
    build_request: impl FnOnce(String) -> Request,
) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let Some(task_id) = parsed.positional.first().cloned() else {
        eprintln!("error: a task id is required");
        return ExitCode::from(64);
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("task signal", &e),
    };
    match client.call(build_request(task_id)).await {
        Response::Ack => ExitCode::SUCCESS,
        Response::Error(e) => print_error_and_fail("task signal", &e.message),
        other => print_error_and_fail("task signal", &format!("unexpected response: {other:?}")),
    }
}

/// Resuming spawns a *new* driver (embedded) or asks the daemon to; either
/// way this waits for real progress, the same as `run`.
async fn cmd_task_resume(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let Some(task_id) = parsed.positional.first().cloned() else {
        eprintln!("error: `valyria task resume` needs a task id");
        return ExitCode::from(64);
    };
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("task resume", &e),
    };
    let resume_task_id = task_id.clone();
    let client_ref = client.clone();
    watch_established_task_to_terminal(client.as_ref(), &task_id, parsed.events, async move {
        match client_ref
            .call(Request::TaskResume(TaskIdRequest {
                task_id: resume_task_id,
            }))
            .await
        {
            Response::Ack => Ok(()),
            Response::Error(e) => Err(print_error_and_fail("task resume", &e.message)),
            other => Err(print_error_and_fail(
                "task resume",
                &format!("unexpected response: {other:?}"),
            )),
        }
    })
    .await
}

async fn cmd_permission_resolve(raw: Vec<String>) -> ExitCode {
    let parsed = match parse(&raw) {
        Ok(p) => p,
        Err(e) => return print_error_and_fail("invalid arguments", &e),
    };
    let Some(task_id) = parsed.positional.first().cloned() else {
        eprintln!("error: `valyria task permission resolve` needs a task id");
        return ExitCode::from(64);
    };
    if parsed.allow == parsed.deny {
        eprintln!("error: exactly one of --allow or --deny is required");
        return ExitCode::from(64);
    }
    let client = match build_client(&parsed).await {
        Ok(c) => c,
        Err(e) => return print_error_and_fail("permission resolve", &e),
    };
    let resolve_task_id = task_id.clone();
    let approve = parsed.allow;
    let client_ref = client.clone();
    watch_established_task_to_terminal(client.as_ref(), &task_id, parsed.events, async move {
        match client_ref
            .call(Request::PermissionResolve(PermissionResolveRequest {
                task_id: resolve_task_id,
                approve,
            }))
            .await
        {
            Response::Ack => Ok(()),
            Response::Error(e) => Err(print_error_and_fail("permission resolve", &e.message)),
            other => Err(print_error_and_fail(
                "permission resolve",
                &format!("unexpected response: {other:?}"),
            )),
        }
    })
    .await
}
