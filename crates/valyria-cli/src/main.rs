//! `valyria` — the CLI. A thin protocol client (layer 6, D11): every
//! command below goes through `valyria_protocol::Client` against an
//! embedded `valyria_app::Runtime` — this binary contains no state-machine,
//! journal, or tool-invocation logic of its own, and its `Cargo.toml`
//! lists only `valyria-app`/`valyria-protocol`/`valyria-types`/
//! `valyria-util`. Full command surface (a daemon `--connect` mode,
//! `--json` everywhere, shell completions) lands in Phase 10; this is the
//! walking skeleton's `run`/`task` surface.

mod args;

use std::process::ExitCode;
use std::sync::Arc;

use futures::StreamExt;
use valyria_app::{load_scenario, EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{
    Client, PermissionResolveRequest, Request, Response, TaskCreateRequest, TaskIdRequest,
    TaskStatusRequest,
};

use args::{parse, resolve_workspace, ParsedArgs};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("valyria {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("run") => tokio_main(cmd_run(argv[1..].to_vec())),
        Some("task") => tokio_main(cmd_task(argv[1..].to_vec())),
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
    eprintln!(
        "    valyria run \"<objective>\" [--workspace <path>] [--scenario <file.toml>] \
         [--permission-mode manual|assisted|autonomous] [--events]"
    );
    eprintln!("    valyria task status <task_id> [--workspace <path>]");
    eprintln!("    valyria task pause <task_id> [--workspace <path>]");
    eprintln!("    valyria task resume <task_id> [--workspace <path>]");
    eprintln!("    valyria task cancel <task_id> [--workspace <path>]");
    eprintln!(
        "    valyria task permission resolve <task_id> (--allow|--deny) [--workspace <path>]"
    );
}

async fn build_client(parsed: &ParsedArgs) -> Result<EmbeddedClient, String> {
    let workspace_path = resolve_workspace(parsed);
    let mut config = RuntimeConfig::new(workspace_path);
    if let Some(mode) = parsed.permission_mode {
        config = config.with_permission_mode(mode);
    }
    if let Some(path) = &parsed.scenario {
        let scenario = load_scenario(path).map_err(|e| format!("failed to load scenario: {e}"))?;
        config = config.with_scenario(scenario);
    }
    let runtime = Runtime::open(config)
        .await
        .map_err(|e| format!("failed to open runtime: {e}"))?;
    Ok(EmbeddedClient::new(Arc::new(runtime)))
}

fn print_error_and_fail(context: &str, message: &str) -> ExitCode {
    eprintln!("error: {context}: {message}");
    ExitCode::FAILURE
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
        .call(Request::TaskCreate(TaskCreateRequest { objective }))
        .await;
    let task_id = match response {
        Response::TaskCreate(r) => r.task_id,
        Response::Error(e) => return print_error_and_fail("task create", &e.message),
        other => {
            return print_error_and_fail("task create", &format!("unexpected response: {other:?}"))
        }
    };
    println!("task_id: {task_id}");
    // Explicit flush: stdout is block-buffered (not line-buffered) once
    // piped rather than attached to a terminal, so without this a reader
    // waiting on this line — including a test that kills this process
    // immediately afterward to exercise crash recovery — could otherwise
    // never see it.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    watch_task_to_terminal(&client, &task_id, parsed.events).await
}

/// Streams events from `since: 0` and watches this task's `state_changed`
/// until it reaches a state worth stopping at, printing a summary and
/// returning the matching exit code.
///
/// Used right after a request that starts a fresh driver in *this*
/// process (`run`'s `TaskCreate`): without waiting here, a
/// `tokio::spawn`ed driver would be silently abandoned the moment `main`
/// returns and the process exits. Safe to start from `since: 0` here
/// specifically because task creation is this task's very first event —
/// there is no earlier history to misinterpret as fresh (contrast
/// `watch_established_task_to_terminal`, used after `resume`/`permission
/// resolve`, where there is).
async fn watch_task_to_terminal(
    client: &EmbeddedClient,
    task_id: &str,
    print_events: bool,
) -> ExitCode {
    let events = client.subscribe_events(0).await;
    watch_stream_to_terminal(client, events, task_id, print_events).await
}

/// Same as `watch_task_to_terminal`, but for a task that may already have
/// history *before* the state transition we actually care about (e.g. a
/// prior `WAITING_FOR_PERMISSION` from an earlier `run`/`resume`
/// invocation) — subscribing from `since: 0` and reacting to the first
/// matching `state_changed` naively would immediately "match" that old
/// event and report stale results. Subscribes first, then drains whatever
/// is already durably backlogged (bounded by a short per-read timeout,
/// safe because `Subscription`'s backlog is an already-in-memory `VecDeque`
/// read once at subscribe time — draining it never blocks on real
/// work), *then* issues `after_subscribing` (the resume/resolve request
/// itself), so everything read afterward is guaranteed to be new.
async fn watch_established_task_to_terminal<F>(
    client: &EmbeddedClient,
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
    client: &EmbeddedClient,
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
                // `PAUSED` can be transient: `resume_task`'s crash-recovery
                // step legitimately passes a task through `Paused` for a
                // moment (stuck-state -> Paused -> the state it resumes
                // into is the only path the state machine allows — direct
                // self-transitions are forbidden, so this two-step shape
                // is structural, not a bug) before immediately continuing
                // it. Confirm against the *current* live status before
                // treating this as the task actually being at rest — an
                // event from history saying "it was paused" is not the
                // same as "it still is."
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
                    _ => {} // already moved on — keep watching
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
    match client
        .call(Request::TaskStatus(TaskStatusRequest { task_id }))
        .await
    {
        Response::TaskStatus(status) => {
            println!("task_id: {}", status.task_id);
            println!("objective: {}", status.objective);
            println!("state: {}", status.state);
            if let Some(paused_from) = status.paused_from {
                println!("paused_from: {paused_from}");
            }
            if let Some(note) = status.recovery_note {
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

/// Unlike `pause`/`cancel` (durable, fire-and-forget requests another
/// process's driver will notice), resuming spawns a *new* driver in *this*
/// process (there is no other process already running one — that's the
/// point of resuming). So this waits for it to actually progress, the same
/// way `run` does, rather than returning the moment the resume request is
/// acked.
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
    watch_established_task_to_terminal(&client, &task_id, parsed.events, async {
        match client
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
    // Approving may let the driver run all the way to completion, on a
    // task `tokio::spawn`ed inside *this* process — wait for it (same
    // reasoning as `cmd_task_resume`) rather than exiting immediately and
    // abandoning it. A denial resolves to `Failed` almost instantly, so
    // this returns quickly either way.
    watch_established_task_to_terminal(&client, &task_id, parsed.events, async {
        match client
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
