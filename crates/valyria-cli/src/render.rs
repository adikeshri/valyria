//! Human-readable rendering of protocol `Response`s. The CLI's only job
//! past the `Client` call is presentation — no logic, no state.

use valyria_protocol::Response;

/// `--json`: the response's payload as pretty JSON. For a `{result,
/// value}` envelope we print just `value`; `Ack` and anything unusual
/// print whole.
pub fn to_json(response: &Response) -> String {
    let full = serde_json::to_value(response).unwrap_or(serde_json::Value::Null);
    let payload = full.get("value").cloned().unwrap_or(full);
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "null".to_string())
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn doctor(response: &Response) {
    let Response::DoctorRun(r) = response else {
        return;
    };
    for c in &r.checks {
        let mark = match c.status.as_str() {
            "pass" => "✓",
            "warn" => "!",
            _ => "✗",
        };
        println!("{mark} {:<18} {}", c.name, c.detail);
        if let Some(fix) = &c.remediation {
            println!("    → {fix}");
        }
    }
    println!("\nsummary: {}", r.summary);
}

pub fn workspace_status(response: &Response) {
    let Response::WorkspaceStatus(r) = response else {
        return;
    };
    println!("workspace_id:      {}", r.workspace_id);
    println!("root:              {}", r.root);
    println!("data_dir:          {}", r.data_dir);
    println!(
        "index_generation:  {}",
        r.index_generation
            .map(|g| g.to_string())
            .unwrap_or_else(|| "none (not indexed yet)".to_string())
    );
    println!(
        "tasks:             {} active / {} total",
        r.active_tasks, r.total_tasks
    );
}

pub fn config_show(response: &Response) {
    let Response::ConfigShow(r) = response else {
        return;
    };
    for e in &r.entries {
        println!("{:<20} {:<24} ({})", e.key, e.value, e.origin);
    }
}

pub fn model_list(response: &Response) {
    let Response::ModelList(r) = response else {
        return;
    };
    if r.models.is_empty() {
        println!("(no models in the catalog)");
        return;
    }
    for m in &r.models {
        let flag = if m.installed {
            "installed"
        } else {
            "        -"
        };
        let roles = if m.active_roles.is_empty() {
            String::new()
        } else {
            format!("  [{}]", m.active_roles.join(", "))
        };
        println!(
            "{flag}  {:<40} {:<8} {:>10}  {}{roles}",
            m.id,
            m.quantization,
            bytes(m.size_bytes),
            m.license
        );
    }
}

pub fn model_recommend(response: &Response) {
    let Response::ModelRecommend(r) = response else {
        return;
    };
    println!("role: {}", r.role);
    match &r.recommended {
        Some(c) => println!("recommended: {} ({})", c.display_name, c.id),
        None => println!("recommended: (nothing on this machine fits)"),
    }
    println!();
    for c in &r.candidates {
        let mark = if c.installed { "*" } else { " " };
        let detail = c.fit_detail.as_deref().unwrap_or("");
        println!(
            "{mark} {:<40} {:>10}  {:<13} {:<20} suit {}",
            c.id,
            bytes(c.size_bytes),
            c.fit_kind,
            detail,
            c.suitability
        );
    }
}

pub fn model_inspect(response: &Response) {
    let Response::ModelInspect(m) = response else {
        return;
    };
    println!("{} ({})", m.display_name, m.id);
    println!("  family        {}", m.family);
    println!("  parameters    {:.1} B", m.parameters_b);
    println!("  quantization  {}", m.quantization);
    println!("  context       {}", m.context_length);
    println!("  size          {}", bytes(m.size_bytes));
    println!("  license       {}", m.license_name);
    if let Some(url) = &m.license_url {
        println!("                {url}");
    }
    println!(
        "  license text  {}",
        if m.license_text.is_some() {
            "bundled (shown at install)"
        } else {
            "not bundled"
        }
    );
    println!("  installed     {}", m.installed);
    if let Some(ts) = m.installed_at_ms {
        println!("  installed at  {ts} ms");
    }
    if let Some(ts) = m.license_accepted_at_ms {
        println!("  license ok at {ts} ms");
    }
    if let Some(tps) = m.probe_tokens_per_sec {
        println!("  probe         {tps:.1} tok/s");
    }
    if !m.active_roles.is_empty() {
        println!("  active roles  {}", m.active_roles.join(", "));
    }
}

pub fn model_remove(response: &Response) {
    let Response::ModelRemove(r) = response else {
        return;
    };
    println!("removed — {} reclaimed", bytes(r.freed_bytes));
}

pub fn memory_list(response: &Response) {
    let Response::MemoryList(r) = response else {
        return;
    };
    if r.entries.is_empty() {
        println!("(no matching memory)");
        return;
    }
    for e in &r.entries {
        println!(
            "[{:<10} {:<6} conf {:.2}] {}",
            e.scope, e.author, e.effective_confidence, e.text
        );
    }
}

pub fn purge(response: &Response) {
    let Response::Purge(r) = response else {
        return;
    };
    let verb = if r.dry_run { "would free" } else { "freed" };
    if r.freed_bytes > 0 {
        println!(
            "{verb} {} across {} item(s)",
            bytes(r.freed_bytes),
            r.items_removed
        );
    } else {
        println!("{verb} {} item(s)", r.items_removed);
    }
}

pub fn task_list(response: &Response) {
    let Response::TaskList(r) = response else {
        return;
    };
    if r.tasks.is_empty() {
        println!("(no tasks in this workspace)");
        return;
    }
    for t in &r.tasks {
        println!("{:<30} {:<22} {}", t.task_id, t.state, t.objective);
    }
}

pub fn task_report(response: &Response) {
    let Response::TaskReport(r) = response else {
        return;
    };
    println!("task {}", r.task_id);
    println!("verification: {}", r.status);
    for v in &r.verified {
        println!("  ✓ {:<8} {} [{}]", v.kind, v.command, v.run_id);
    }
    for u in &r.unverified {
        println!("  ✗ {u}");
    }
}

pub fn task_plan(response: &Response) {
    let Response::TaskPlan(r) = response else {
        return;
    };
    match r.revision {
        None => println!("(this task ran without a model-authored plan)"),
        Some(rev) => {
            println!(
                "plan revision {rev}  ({} step(s), hash {})",
                r.steps.len(),
                r.content_hash.as_deref().unwrap_or("-")
            );
            for s in &r.steps {
                let marks = match (s.rollback_boundary, s.checkpoint) {
                    (true, true) => " [rollback+checkpoint]",
                    (true, false) => " [rollback]",
                    (false, true) => " [checkpoint]",
                    _ => "",
                };
                println!("  {} — {}{}", s.id, s.intent, marks);
                if !s.depends_on.is_empty() {
                    println!("      depends on: {}", s.depends_on.join(", "));
                }
                if !s.targets.is_empty() {
                    println!("      targets: {}", s.targets.join(", "));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_protocol::{DoctorCheckWire, DoctorRunResponse, PurgeResponse};

    #[test]
    fn bytes_scales() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2.0 KiB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn to_json_unwraps_the_value_envelope() {
        let resp = Response::Purge(PurgeResponse {
            freed_bytes: 10,
            items_removed: 2,
            dry_run: true,
        });
        let json = to_json(&resp);
        assert!(json.contains("\"freed_bytes\": 10"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn to_json_handles_ack() {
        // No `value` envelope on `Ack` — fall back to the whole response.
        assert!(to_json(&Response::Ack).contains("ack"));
    }

    #[test]
    fn doctor_renders_without_panicking() {
        let resp = Response::DoctorRun(DoctorRunResponse {
            checks: vec![DoctorCheckWire {
                name: "git".into(),
                status: "warn".into(),
                detail: "not a git repository".into(),
                remediation: Some("run `git init`".into()),
            }],
            summary: "warn".into(),
        });
        doctor(&resp);
    }
}
