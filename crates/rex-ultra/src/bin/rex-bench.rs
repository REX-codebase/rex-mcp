//! rex-bench: fixed-suite, blind-scored comparison of raw vs Simple vs Ultra
//! on one pinned worker model.
//!
//!   rex-bench run --suite bench/suites/core.jsonl --mode ultra \
//!       --provider gemini --model gemini-3.5-flash-lite --out results.jsonl
//!   rex-bench report results-a.jsonl results-b.jsonl ...
//!
//! Keys come from the normal REX credential store (same safe provider routes
//! as the app). Every task runs in a fresh disposable workspace; scoring is
//! deterministic re-execution only.

use rex_providers::autonomous::AutonomousRunService;
use rex_providers::oneshot;
use rex_providers::secrets::FileSecretStore;
use rex_providers::service::ProviderService;
use rex_providers::http::UreqTransport;
use rex_ultra::bench::{self, BenchMode, BenchTask, TaskResult};
use rex_ultra::orchestrator::{UltraOptions, UltraRunService};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Args {
    command: String,
    suite: Option<PathBuf>,
    mode: BenchMode,
    provider: String,
    model: Option<String>,
    out: PathBuf,
    runs_root: PathBuf,
    files: Vec<PathBuf>,
    task_filter: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let mut parsed = Args {
        command,
        suite: None,
        mode: BenchMode::Raw,
        provider: "gemini".into(),
        model: None,
        out: PathBuf::from("bench-results.jsonl"),
        runs_root: std::env::temp_dir().join(format!("rex-bench-{}", std::process::id())),
        files: Vec::new(),
        task_filter: None,
    };
    let mut rest: Vec<String> = args.collect();
    while !rest.is_empty() {
        let flag = rest.remove(0);
        let mut value = |name: &str| -> Result<String, String> {
            if rest.is_empty() {
                return Err(format!("{name} needs a value"));
            }
            Ok(rest.remove(0))
        };
        match flag.as_str() {
            "--suite" => parsed.suite = Some(PathBuf::from(value("--suite")?)),
            "--mode" => {
                parsed.mode = match value("--mode")?.as_str() {
                    "raw" => BenchMode::Raw,
                    "simple" => BenchMode::Simple,
                    "ultra" => BenchMode::Ultra,
                    other => return Err(format!("unknown mode {other}")),
                }
            }
            "--provider" => parsed.provider = value("--provider")?,
            "--model" => parsed.model = Some(value("--model")?),
            "--out" => parsed.out = PathBuf::from(value("--out")?),
            "--runs-root" => parsed.runs_root = PathBuf::from(value("--runs-root")?),
            "--task" => parsed.task_filter = Some(value("--task")?),
            other if !other.starts_with("--") => parsed.files.push(PathBuf::from(other)),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(parsed)
}

fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("rex-harness");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("rex-harness");
    }
    std::env::temp_dir().join("rex-harness")
}

fn run_task(
    task: &BenchTask,
    args: &Args,
    agent: &Arc<AutonomousRunService<FileSecretStore, UreqTransport>>,
    ultra: &Arc<UltraRunService<FileSecretStore, UreqTransport>>,
    service: &ProviderService<FileSecretStore, UreqTransport>,
) -> TaskResult {
    let started = Instant::now();
    let workspace = args.runs_root.join(format!("{}-workspace", task.id));
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).expect("workspace");
    let evidence_dir = args.runs_root.join(format!("{}-evidence", task.id));
    let _ = std::fs::remove_dir_all(&evidence_dir);

    let (tokens, steps, approvals, terminal) = match args.mode {
        BenchMode::Raw => {
            let model = match &args.model {
                Some(m) => m.clone(),
                None => match oneshot::resolve_model(service, &args.provider, None) {
                    Ok(m) => m,
                    Err(e) => {
                        return TaskResult {
                            task_id: task.id.clone(),
                            mode: args.mode,
                            passed: false,
                            checks: Vec::new(),
                            tokens_used: 0,
                            wall_ms: started.elapsed().as_millis() as u64,
                            steps: 0,
                            approvals: 0,
                            terminal: format!("model resolution failed: {e}"),
                        }
                    }
                },
            };
            match oneshot::complete_text(
                service,
                &args.provider,
                &model,
                "You produce complete file-based solutions.",
                &bench::raw_prompt(task),
            ) {
                Ok(out) => {
                    let files = bench::extract_raw_files(&out.text);
                    let _ = bench::write_files(&workspace, &files);
                    (out.tokens, 1, 0, "single_shot".to_string())
                }
                Err(e) => (0, 0, 0, format!("provider_error: {e}")),
            }
        }
        BenchMode::Simple => {
            let snap = agent
                .begin_in_workspace(
                    &task.prompt,
                    &args.provider,
                    args.model.as_deref(),
                    None,
                    Some(workspace.clone()),
                )
                .expect("begin");
            let mut approvals = 0u32;
            let final_snap = loop {
                let s = agent.snapshot(&snap.id).expect("snapshot");
                if s.pending_approval.is_some() {
                    approvals += 1;
                    let _ = agent.decide(&snap.id, true);
                }
                if s.terminal_reason.is_some() {
                    break s;
                }
                std::thread::sleep(Duration::from_millis(250));
            };
            (
                final_snap.tokens_used,
                final_snap.step,
                approvals,
                format!("{:?}", final_snap.terminal_reason),
            )
        }
        BenchMode::Ultra => {
            let snap = ultra
                .begin(
                    &task.prompt,
                    &args.provider,
                    args.model.as_deref(),
                    UltraOptions::default(),
                )
                .expect("ultra begin");
            let mut approvals = 0u32;
            let mut last_tokens = 0u64;
            let mut last_steps = 0usize;
            let terminal = loop {
                let s = ultra.snapshot(&snap.id).expect("snapshot");
                if let Some(builder) = &s.builder {
                    last_tokens = last_tokens.max(builder.tokens_used);
                    last_steps = last_steps.max(builder.step);
                    if builder.pending_approval.is_some() {
                        approvals += 1;
                        let _ = ultra.decide(&snap.id, true);
                    }
                }
                if let Some(t) = &s.terminal {
                    break format!("{t:?}");
                }
                std::thread::sleep(Duration::from_millis(250));
            };
            (last_tokens, last_steps, approvals, terminal)
        }
    };

    let report = bench::score_workspace(task, &workspace, &evidence_dir);
    TaskResult {
        task_id: task.id.clone(),
        mode: args.mode,
        passed: report.executable_all_proven
            && matches!(args.mode, BenchMode::Raw | BenchMode::Simple)
            || (matches!(args.mode, BenchMode::Ultra)
                && report.executable_all_proven
                && terminal.starts_with("Promoted")),
        checks: report.outcomes,
        tokens_used: tokens,
        wall_ms: started.elapsed().as_millis() as u64,
        steps,
        approvals,
        terminal,
    }
}

fn report(files: &[PathBuf]) -> Result<(), String> {
    for file in files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let mut results: Vec<TaskResult> = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            results.push(serde_json::from_str(line).map_err(|e| format!("bad result: {e}"))?);
        }
        let mut by_mode: std::collections::BTreeMap<String, (usize, usize, u64, u64)> =
            std::collections::BTreeMap::new();
        for r in &results {
            let key = format!("{:?}", r.mode).to_lowercase();
            let entry = by_mode.entry(key).or_default();
            entry.0 += usize::from(r.passed);
            entry.1 += 1;
            entry.2 += r.tokens_used;
            entry.3 += r.wall_ms;
        }
        println!("== {}", file.display());
        for (mode, (passed, total, tokens, wall)) in &by_mode {
            println!(
                "  {mode}: {passed}/{total} passed ({:.0}%), {} tokens, {:.1}s mean",
                100.0 * *passed as f64 / (*total).max(1) as f64,
                tokens,
                *wall as f64 / 1000.0 / (*total).max(1) as f64
            );
        }
    }
    Ok(())
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rex-bench: {e}");
            std::process::exit(2);
        }
    };
    if args.command == "report" {
        if let Err(e) = report(&args.files) {
            eprintln!("rex-bench: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.command != "run" {
        eprintln!("rex-bench: expected `run` or `report`");
        std::process::exit(2);
    }
    let suite_path = args.suite.clone().unwrap_or_else(|| PathBuf::from("bench/suite.jsonl"));
    let mut tasks = bench::load_suite(&suite_path).unwrap_or_else(|e| {
        eprintln!("rex-bench: {e}");
        std::process::exit(2);
    });
    if let Some(filter) = &args.task_filter {
        tasks.retain(|t| &t.id == filter);
    }
    let store = FileSecretStore::new(config_dir()).expect("credential store");
    let service = ProviderService::new(store, UreqTransport::new());
    let agent = Arc::new(AutonomousRunService::new(
        service,
        None,
        args.runs_root.clone(),
    ));
    let ultra = Arc::new(UltraRunService::new(agent.clone(), args.runs_root.clone()));
    for task in &tasks {
        let result = run_task(task, &args, &agent, &ultra, agent.service());
        println!(
            "{} {} -> {} ({} tokens, {:.1}s)",
            format!("{:?}", result.mode).to_lowercase(),
            result.task_id,
            if result.passed { "PASS" } else { "FAIL" },
            result.tokens_used,
            result.wall_ms as f64 / 1000.0
        );
        if let Err(e) = bench::append_result(&args.out, &result) {
            eprintln!("rex-bench: could not write result: {e}");
        }
    }
}
