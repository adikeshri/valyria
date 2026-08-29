//! `valyria-bench` — run the offline fixture evaluation suite.
//!
//! Deliberately its own binary, not a `valyria` subcommand: `valyria-cli`
//! may only speak the protocol (D11), and the harness stands up runtimes
//! directly. `cargo xtask bench` wraps this for CI.
//!
//! Usage:
//!   valyria-bench run                     run the suite, print a table
//!   valyria-bench run --json              print the report as JSON
//!   valyria-bench run --baseline <file>   compare to a recorded baseline
//!   valyria-bench baseline <file>         run and (over)write the baseline

use std::process::ExitCode;

use valyria_bench::{compare, fixture_suite, BenchReport, BenchRunner};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("valyria-bench: could not start tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    match args.first().map(String::as_str) {
        Some("run") => rt.block_on(cmd_run(&args[1..])),
        Some("baseline") => rt.block_on(cmd_baseline(&args[1..])),
        _ => {
            eprintln!(
                "usage:\n  valyria-bench run [--json] [--baseline <file>]\n  valyria-bench baseline <file>"
            );
            ExitCode::FAILURE
        }
    }
}

async fn run_suite() -> Result<BenchReport, ExitCode> {
    let runner = BenchRunner::new();
    match runner.run_suite(&fixture_suite()).await {
        Ok(report) => Ok(report),
        Err(e) => {
            eprintln!("valyria-bench: suite errored ({}): {e}", e.code());
            Err(ExitCode::FAILURE)
        }
    }
}

async fn cmd_run(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let baseline_path = flag_value(args, "--baseline");

    let report = match run_suite().await {
        Ok(r) => r,
        Err(code) => return code,
    };

    if json {
        println!("{}", report.to_json_pretty());
    } else {
        print!("{}", report.render_table());
    }

    let mut ok = report.all_passed();

    if let Some(path) = baseline_path {
        match std::fs::read_to_string(&path).map(|s| BenchReport::from_json(&s)) {
            Ok(Ok(baseline)) => {
                let cmp = compare(&baseline, &report);
                print!("{}", cmp.render());
                ok &= cmp.is_clean();
            }
            Ok(Err(e)) => {
                eprintln!("valyria-bench: baseline {path} is not a valid report: {e}");
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("valyria-bench: cannot read baseline {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

async fn cmd_baseline(args: &[String]) -> ExitCode {
    let Some(path) = args.first() else {
        eprintln!("usage: valyria-bench baseline <file>");
        return ExitCode::FAILURE;
    };
    let report = match run_suite().await {
        Ok(r) => r,
        Err(code) => return code,
    };
    print!("{}", report.render_table());
    match std::fs::write(path, format!("{}\n", report.stabilized().to_json_pretty())) {
        Ok(()) => {
            println!("wrote baseline -> {path}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("valyria-bench: cannot write {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
