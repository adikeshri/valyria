//! `valyria` — the CLI. A thin protocol client (layer 6).
//!
//! Per the build plan (D11), this binary must never grow orchestration logic:
//! it only speaks the local protocol (`valyria-protocol`) against an embedded
//! or daemon runtime. Full command surface lands in Phase 10; the `run`
//! command lands with the walking skeleton in Phase 3.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("valyria {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            println!(
                "valyria {} (scaffold — see docs/PLAN.md)",
                env!("CARGO_PKG_VERSION")
            );
            println!("commands are wired in as their owning phases land");
        }
    }
}
