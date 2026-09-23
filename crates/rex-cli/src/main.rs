//! `rex`: the REX headless CLI.
//!
//! Today: `rex exec` (scriptable agent runs for CI), `rex keygen` and
//! `rex verify` (signed run certificates). The desktop app keeps the
//! interactive surface; this binary is the machine surface.

mod bid;
mod cert;
mod deadman;
mod exec;
mod ledger;
mod policy;
mod provenance;
mod redteam;
mod replay;
mod skill;
mod tournament;

use cert::{keygen, verify_receipt};
use exec::{run_exec, state_dir, ExecError, ExecOptions};
use redteam::{run_redteam, RedteamOptions};
use std::process::ExitCode;
use tournament::{run_tournament, TournamentOptions};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> &'static str {
    "rex: headless REX harness CLI\n\
     \n\
     usage:\n\
     \x20 rex exec [--task TASK | TASK...] [options]\n\
     \x20 rex tournament --task TASK --providers a,b [options]\n\
     \x20 rex redteam --task TASK --yes [options]\n\
     \x20 rex policy --workspace DIR\n\
     \x20 rex replay RECEIPT.json [--json]\n\
     \x20 rex checkin --file PATH\n\
     \x20 rex skill install DIR [--force] | list | show NAME | verify [NAME]\n\
     \x20             | remove NAME | pack DIR\n\
     \x20 rex runs [--json] [--limit N]\n\
     \x20 rex show RUN_ID [--json]\n\
     \x20 rex keygen [--force]\n\
     \x20 rex verify RECEIPT.json [--public-key BASE64]\n\
     \x20 rex --version\n\
     \n\
     options:\n\
     \x20 --task TEXT          task description (or pass as trailing words)\n\
     \x20 --provider NAME      gemini|anthropic|openai|xai|deepseek|kimi|kimi-coding|qwen|glm|local\n\
     \x20                      (default: $REX_PROVIDER or anthropic)\n\
     \x20 --model ID           model id (default: $REX_MODEL or provider default)\n\
     \x20 --workspace DIR      directory to copy into the run sandbox\n\
     \x20 --max-steps N        step budget\n\
     \x20 --max-tool-calls N   tool-call budget\n\
     \x20 --max-tokens N       token budget\n\
     \x20 --timeout-secs N     wall-clock budget in seconds\n\
     \x20 --json               print exactly one JSON receipt on stdout\n\
     \x20 --yes                auto-approve tool calls and plans (CI mode)\n\
     \x20 --dry-run            validate args and print config without running\n\
     \x20 --bid                print the binding bid and stop (needs --accept-bid)\n\
     \x20 --accept-bid         run under the printed bid (requires --bid)\n\
     \x20 --budget-usd X        dollar cap, converted worst-case to a token budget\n\
     \x20                      (requires --model with a known price)\n\
     \n\
     credentials: $REX_<PROVIDER>_API_KEY wins, then $REX_API_KEY;\n\
     otherwise the file credential store under $REX_STATE_DIR\n\
     (default ~/.rex/harness) is used.\n\
     \n\
     exit codes: 0 completed · 2 usage/config/approval needed ·\n\
     \x203 run ended without completing · 1 internal error\n"
}

fn parse_usize(raw: &str, flag: &str) -> Result<usize, ExecError> {
    raw.parse::<usize>()
        .map_err(|_| ExecError::usage(format!("{flag} expects a positive integer, got '{raw}'")))
}

fn parse_u64(raw: &str, flag: &str) -> Result<u64, ExecError> {
    raw.parse::<u64>()
        .map_err(|_| ExecError::usage(format!("{flag} expects a positive integer, got '{raw}'")))
}

fn parse_f64(raw: &str, flag: &str) -> Result<f64, ExecError> {
    raw.parse::<f64>()
        .map_err(|_| ExecError::usage(format!("{flag} expects a number, got '{raw}'")))
}

fn parse_exec(args: &[String]) -> Result<ExecOptions, ExecError> {
    let mut opts = ExecOptions::default();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let take_value = |flag: &str, i: &mut usize| -> Result<String, ExecError> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| ExecError::usage(format!("{flag} expects a value")))
        };
        match a {
            "--task" => opts.task = take_value("--task", &mut i)?,
            "--provider" => opts.provider = take_value("--provider", &mut i)?,
            "--model" => opts.model = Some(take_value("--model", &mut i)?),
            "--workspace" => {
                opts.workspace = Some(take_value("--workspace", &mut i)?.into());
            }
            "--max-steps" => {
                opts.max_steps = Some(parse_usize(
                    &take_value("--max-steps", &mut i)?,
                    "--max-steps",
                )?)
            }
            "--max-tool-calls" => {
                opts.max_tool_calls = Some(parse_usize(
                    &take_value("--max-tool-calls", &mut i)?,
                    "--max-tool-calls",
                )?);
            }
            "--max-tokens" => {
                opts.max_tokens = Some(parse_u64(
                    &take_value("--max-tokens", &mut i)?,
                    "--max-tokens",
                )?);
            }
            "--timeout-secs" => {
                opts.timeout_secs = Some(parse_u64(
                    &take_value("--timeout-secs", &mut i)?,
                    "--timeout-secs",
                )?);
            }
            "--json" => opts.json = true,
            "--yes" => opts.yes = true,
            "--dry-run" => opts.dry_run = true,
            "--bid" => opts.bid = true,
            "--accept-bid" => opts.accept_bid = true,
            "--budget-usd" => {
                opts.budget_usd = Some(parse_f64(
                    &take_value("--budget-usd", &mut i)?,
                    "--budget-usd",
                )?);
            }
            "--deadman-mins" => {
                opts.deadman_mins = Some(parse_u64(
                    &take_value("--deadman-mins", &mut i)?,
                    "--deadman-mins",
                )?);
            }
            "--deadman-file" => {
                opts.deadman_file = Some(take_value("--deadman-file", &mut i)?.into());
            }
            "--skill" => {
                opts.skills.push(take_value("--skill", &mut i)?);
            }
            "--help" | "-h" => return Err(ExecError::usage(usage())),
            other if other.starts_with('-') => {
                return Err(ExecError::usage(format!("unknown flag '{other}'")));
            }
            _ => positional.push(a.to_string()),
        }
        i += 1;
    }
    if opts.task.is_empty() && !positional.is_empty() {
        opts.task = positional.join(" ");
    }
    if opts.accept_bid && !opts.bid {
        return Err(ExecError::usage("--accept-bid requires --bid"));
    }
    Ok(opts)
}

fn parse_tournament(args: &[String]) -> Result<TournamentOptions, ExecError> {
    let mut opts = TournamentOptions {
        task: String::new(),
        providers: Vec::new(),
        model: None,
        workspace: None,
        max_steps: None,
        max_tool_calls: None,
        max_tokens: None,
        timeout_secs: None,
        json: false,
        yes: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let take_value = |flag: &str, i: &mut usize| -> Result<String, ExecError> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| ExecError::usage(format!("{flag} expects a value")))
        };
        match a {
            "--task" => opts.task = take_value("--task", &mut i)?,
            "--providers" => {
                let raw = take_value("--providers", &mut i)?;
                let mut seen = std::collections::HashSet::new();
                opts.providers = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .filter(|p| seen.insert(p.to_string()))
                    .map(str::to_string)
                    .collect();
            }
            "--model" => opts.model = Some(take_value("--model", &mut i)?),
            "--workspace" => {
                opts.workspace = Some(take_value("--workspace", &mut i)?.into());
            }
            "--max-steps" => {
                opts.max_steps = Some(parse_usize(
                    &take_value("--max-steps", &mut i)?,
                    "--max-steps",
                )?)
            }
            "--max-tool-calls" => {
                opts.max_tool_calls = Some(parse_usize(
                    &take_value("--max-tool-calls", &mut i)?,
                    "--max-tool-calls",
                )?)
            }
            "--max-tokens" => {
                opts.max_tokens = Some(parse_u64(
                    &take_value("--max-tokens", &mut i)?,
                    "--max-tokens",
                )?)
            }
            "--timeout-secs" => {
                opts.timeout_secs = Some(parse_u64(
                    &take_value("--timeout-secs", &mut i)?,
                    "--timeout-secs",
                )?)
            }
            "--json" => opts.json = true,
            "--yes" => opts.yes = true,
            "--help" | "-h" => return Err(ExecError::usage(usage())),
            other if other.starts_with('-') => {
                return Err(ExecError::usage(format!("unknown flag '{other}'")));
            }
            other => {
                return Err(ExecError::usage(format!(
                    "unexpected argument '{other}': pass the task with --task"
                )));
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn parse_redteam(args: &[String]) -> Result<RedteamOptions, ExecError> {
    let mut opts = RedteamOptions {
        task: String::new(),
        provider: String::new(),
        model: None,
        workspace: None,
        max_steps: None,
        max_tool_calls: None,
        max_tokens: None,
        timeout_secs: None,
        json: false,
        yes: false,
        dry_run: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let take_value = |flag: &str, i: &mut usize| -> Result<String, ExecError> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| ExecError::usage(format!("{flag} expects a value")))
        };
        match a {
            "--task" => opts.task = take_value("--task", &mut i)?,
            "--provider" => opts.provider = take_value("--provider", &mut i)?,
            "--model" => opts.model = Some(take_value("--model", &mut i)?),
            "--workspace" => {
                opts.workspace = Some(take_value("--workspace", &mut i)?.into());
            }
            "--max-steps" => {
                opts.max_steps = Some(parse_usize(
                    &take_value("--max-steps", &mut i)?,
                    "--max-steps",
                )?)
            }
            "--max-tool-calls" => {
                opts.max_tool_calls = Some(parse_usize(
                    &take_value("--max-tool-calls", &mut i)?,
                    "--max-tool-calls",
                )?)
            }
            "--max-tokens" => {
                opts.max_tokens = Some(parse_u64(
                    &take_value("--max-tokens", &mut i)?,
                    "--max-tokens",
                )?)
            }
            "--timeout-secs" => {
                opts.timeout_secs = Some(parse_u64(
                    &take_value("--timeout-secs", &mut i)?,
                    "--timeout-secs",
                )?)
            }
            "--json" => opts.json = true,
            "--yes" => opts.yes = true,
            "--dry-run" => opts.dry_run = true,
            "--help" | "-h" => return Err(ExecError::usage(usage())),
            other if other.starts_with('-') => {
                return Err(ExecError::usage(format!("unknown flag '{other}'")));
            }
            other => {
                return Err(ExecError::usage(format!(
                    "unexpected argument '{other}': pass the task with --task"
                )));
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => Err(ExecError::usage(usage())),
        Some("--version") | Some("-V") => {
            println!("rex {VERSION}");
            return ExitCode::from(0);
        }
        Some("exec") => parse_exec(&args[1..]).and_then(run_exec),
        Some("tournament") => parse_tournament(&args[1..]).and_then(run_tournament),
        Some("redteam") => parse_redteam(&args[1..]).and_then(run_redteam),
        Some("policy") => run_policy(&args[1..]),
        Some("replay") => run_replay_args(&args[1..]),
        Some("checkin") => run_checkin(&args[1..]),
        Some("skill") => run_skill(&args[1..]),
        Some("runs") => run_runs(&args[1..]).map(|_| 0),
        Some("show") => run_show(&args[1..]),
        Some("keygen") => run_keygen(&args[1..]).map(|_| 0),
        Some("verify") => run_verify(&args[1..]),
        Some(other) => Err(ExecError::usage(format!(
            "unknown command '{other}'\n{usage}",
            usage = usage()
        ))),
    };
    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("rex: error: {}", e.message);
            ExitCode::from(e.code as u8)
        }
    }
}

fn run_policy(args: &[String]) -> Result<i32, ExecError> {
    let mut workspace: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--workspace" => {
                i += 1;
                workspace = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| ExecError::usage("--workspace expects a value"))?,
                );
            }
            "--help" | "-h" => return Err(ExecError::usage(usage())),
            other => {
                return Err(ExecError::usage(format!("unknown argument '{other}'")));
            }
        }
        i += 1;
    }
    let ws = workspace.ok_or_else(|| ExecError::usage("rex policy --workspace DIR"))?;
    let path = std::path::PathBuf::from(&ws);
    match policy::load(&path).map_err(|e| ExecError::usage(format!("policy: {e}")))? {
        Some(p) => println!("{}", serde_json::to_string_pretty(&p.to_json()).unwrap()),
        None => println!("no policy: {ws} declares no .rex/policy.json contract"),
    }
    Ok(0)
}

fn run_replay_args(args: &[String]) -> Result<i32, ExecError> {
    let mut path: Option<String> = None;
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => return Err(ExecError::usage(usage())),
            other if other.starts_with('-') => {
                return Err(ExecError::usage(format!("unknown flag '{other}'")))
            }
            other => {
                if path.is_some() {
                    return Err(ExecError::usage("rex replay takes one receipt path"));
                }
                path = Some(other.to_string());
            }
        }
    }
    let path = path.ok_or_else(|| ExecError::usage("rex replay RECEIPT.json [--json]"))?;
    replay::run_replay(&path, json)
}

fn run_checkin(args: &[String]) -> Result<i32, ExecError> {
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                i += 1;
                file = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| ExecError::usage("--file expects a value"))?,
                );
            }
            "--help" | "-h" => return Err(ExecError::usage(usage())),
            other => return Err(ExecError::usage(format!("unknown argument '{other}'"))),
        }
        i += 1;
    }
    let file = file.ok_or_else(|| ExecError::usage("rex checkin --file PATH"))?;
    let path = std::path::PathBuf::from(&file);
    deadman::checkin(&path).map_err(ExecError::internal)?;
    println!("checked in at {file}");
    Ok(0)
}

fn run_skill(args: &[String]) -> Result<i32, ExecError> {
    let mut it = args.iter();
    let sub = it
        .next()
        .ok_or_else(|| ExecError::usage("rex skill install|list|show|verify|remove|pack ..."))?;
    let rest: Vec<String> = it.cloned().collect();
    let state = exec::state_dir();
    match sub.as_str() {
        "install" => {
            let mut src: Option<String> = None;
            let mut force = false;
            for a in &rest {
                match a.as_str() {
                    "--force" => force = true,
                    "--help" | "-h" => return Err(ExecError::usage(usage())),
                    other if other.starts_with('-') => {
                        return Err(ExecError::usage(format!("unknown flag '{other}'")))
                    }
                    other => {
                        if src.is_some() {
                            return Err(ExecError::usage("rex skill install DIR [--force]"));
                        }
                        src = Some(other.to_string());
                    }
                }
            }
            let src = src.ok_or_else(|| ExecError::usage("rex skill install DIR [--force]"))?;
            let dest = skill::install(&state, std::path::Path::new(&src), force)
                .map_err(|e| ExecError::internal(e.to_string()))?;
            let lock =
                skill::verify_installed(&dest).map_err(|e| ExecError::internal(e.to_string()))?;
            println!(
                "installed skill '{}' v{} ({} files verified)",
                lock.name,
                lock.version,
                lock.files.len()
            );
            Ok(0)
        }
        "list" => {
            let lib = skill::library_dir(&state);
            let mut names: Vec<String> = Vec::new();
            if lib.exists() {
                let entries =
                    std::fs::read_dir(&lib).map_err(|e| ExecError::internal(e.to_string()))?;
                for e in entries.flatten() {
                    if e.path().is_dir() {
                        if let Some(n) = e.file_name().to_str() {
                            names.push(n.to_string());
                        }
                    }
                }
            }
            names.sort();
            for n in &names {
                match skill::verify_installed(&lib.join(n)) {
                    Ok(lock) => println!("{n} v{} — {}", lock.version, lock.files.len()),
                    Err(_) => println!("{n} (BROKEN: failed verification)"),
                }
            }
            if names.is_empty() {
                println!("no skills installed");
            }
            Ok(0)
        }
        "show" => {
            let name = rest
                .first()
                .ok_or_else(|| ExecError::usage("rex skill show NAME"))?;
            let dir = skill::library_dir(&state).join(name);
            let lock =
                skill::verify_installed(&dir).map_err(|e| ExecError::internal(e.to_string()))?;
            let m = skill::read_manifest(&dir).map_err(|e| ExecError::internal(e.to_string()))?;
            println!("{} v{}", lock.name, lock.version);
            if !m.author.is_empty() {
                println!("author: {}", m.author);
            }
            if !m.description.is_empty() {
                println!("description: {}", m.description);
            }
            println!("entry: {}", m.entry);
            println!("files:");
            for (rel, hash) in &lock.files {
                println!("  {rel}  {hash}");
            }
            Ok(0)
        }
        "verify" => {
            let lib = skill::library_dir(&state);
            let targets: Vec<String> = if rest.is_empty() {
                let mut all = Vec::new();
                if lib.exists() {
                    for e in std::fs::read_dir(&lib)
                        .map_err(|e| ExecError::internal(e.to_string()))?
                        .flatten()
                    {
                        if e.path().is_dir() {
                            if let Some(n) = e.file_name().to_str() {
                                all.push(n.to_string());
                            }
                        }
                    }
                }
                all
            } else {
                rest.clone()
            };
            let mut bad = 0;
            for n in &targets {
                match skill::verify_installed(&lib.join(n)) {
                    Ok(lock) => println!("{n} v{}: OK", lock.version),
                    Err(e) => {
                        println!("{n}: FAILED — {e}");
                        bad += 1;
                    }
                }
            }
            if targets.is_empty() {
                println!("no skills installed");
            }
            Ok(if bad == 0 { 0 } else { 3 })
        }
        "remove" => {
            let name = rest
                .first()
                .ok_or_else(|| ExecError::usage("rex skill remove NAME"))?;
            let dir = skill::library_dir(&state).join(name);
            if !dir.exists() {
                return Err(ExecError::internal(format!(
                    "skill '{name}' is not installed"
                )));
            }
            std::fs::remove_dir_all(&dir).map_err(|e| ExecError::internal(e.to_string()))?;
            println!("removed skill '{name}'");
            Ok(0)
        }
        "pack" => {
            let src = rest
                .first()
                .ok_or_else(|| ExecError::usage("rex skill pack DIR"))?;
            let m = skill::pack(std::path::Path::new(src))
                .map_err(|e| ExecError::internal(e.to_string()))?;
            println!(
                "packed '{}' v{}: {} files hashed into {}",
                m.name,
                m.version,
                m.files.len(),
                skill::MANIFEST_FILE
            );
            Ok(0)
        }
        other => Err(ExecError::usage(format!("unknown skill command '{other}'"))),
    }
}

fn run_runs(args: &[String]) -> Result<(), ExecError> {
    let mut json = false;
    let mut limit: usize = 20;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| ExecError::usage("usage: rex runs [--json] [--limit N]"))?;
            }
            other => {
                return Err(ExecError::usage(format!(
                    "unknown flag '{other}': usage: rex runs [--json] [--limit N]"
                )));
            }
        }
        i += 1;
    }
    let mut entries = ledger::read_all(&state_dir());
    entries.reverse(); // newest first
    let entries: Vec<_> = entries.into_iter().take(limit).collect();
    if json {
        println!("{}", serde_json::to_string(&entries).unwrap());
        return Ok(());
    }
    if entries.is_empty() {
        println!("no runs recorded yet.");
        return Ok(());
    }
    for v in &entries {
        let id = v
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let status = v
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let at = v
            .get("finished_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let task = v
            .get("task")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let task_short: String = task.chars().take(60).collect();
        let who = if ledger::kind_of(v) == "tournament" {
            let w = v
                .get("winner")
                .and_then(|w| w.get("provider"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            format!("tournament→{w}")
        } else {
            v.get("provider")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string()
        };
        println!("{id}  [{status}] {who}  {at}  {task_short}");
    }
    Ok(())
}

fn run_show(args: &[String]) -> Result<i32, ExecError> {
    let mut id: Option<&str> = None;
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(ExecError::usage(format!("unknown flag '{other}'")));
            }
            other => {
                if id.is_some() {
                    return Err(ExecError::usage("usage: rex show RUN_ID [--json]"));
                }
                id = Some(other);
            }
        }
    }
    let id = id.ok_or_else(|| ExecError::usage("usage: rex show RUN_ID [--json]"))?;
    let v = ledger::find(&state_dir(), id).ok_or_else(|| {
        ExecError::usage(format!("no run '{id}' in the ledger (or ambiguous prefix)"))
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(0);
    }
    let get = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or("?");
    let get_u = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
    println!("run        {}", get("run_id"));
    println!("kind       {}", ledger::kind_of(&v));
    println!("task       {}", get("task"));
    println!("provider   {} / {}", get("provider"), get("model"));
    println!("status     {} ({})", get("status"), get("terminal_reason"));
    println!(
        "usage      {} steps, {} tool calls, {} tokens, {} ms",
        get_u("steps"),
        get_u("tool_calls"),
        get_u("tokens_used"),
        get_u("elapsed_ms")
    );
    println!("finished   {}", get("finished_at"));
    if ledger::kind_of(&v) == "tournament" {
        if let Some(w) = v.get("winner") {
            println!(
                "winner     {} — {}",
                w.get("provider")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?"),
                w.get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            );
        }
    } else {
        if let Some(r) = v.get("result").and_then(serde_json::Value::as_str) {
            println!("result     {r}");
        }
        if let Some(e) = v.get("error").and_then(serde_json::Value::as_str) {
            println!("error      {e}");
        }
        if let Some(ws) = v.get("workspace").and_then(serde_json::Value::as_str) {
            println!("workspace  {ws}");
        }
    }
    match verify_receipt(v, None) {
        Ok(r) => println!("certificate valid — signed by {}", r.public_key),
        Err(e) => println!("certificate INVALID: {e}"),
    }
    Ok(0)
}

fn run_keygen(args: &[String]) -> Result<(), ExecError> {
    let force = args.iter().any(|a| a == "--force");
    if args.iter().any(|a| a != "--force") {
        return Err(ExecError::usage("usage: rex keygen [--force]"));
    }
    match keygen(&state_dir(), force) {
        Ok(public_key) => {
            eprintln!("rex: signing key ready.");
            println!("{public_key}");
            Ok(())
        }
        Err(e) => Err(ExecError::internal(format!("keygen failed: {e}"))),
    }
}

fn run_verify(args: &[String]) -> Result<i32, ExecError> {
    let mut path: Option<&str> = None;
    let mut public_key: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--public-key" => {
                i += 1;
                public_key = Some(args.get(i).map(String::as_str).ok_or_else(|| {
                    ExecError::usage("usage: rex verify RECEIPT.json [--public-key BASE64]")
                })?);
            }
            other if other.starts_with('-') => {
                return Err(ExecError::usage(format!("unknown flag '{other}'")));
            }
            p => {
                if path.is_some() {
                    return Err(ExecError::usage(
                        "usage: rex verify RECEIPT.json [--public-key BASE64]",
                    ));
                }
                path = Some(p);
            }
        }
        i += 1;
    }
    let path = path
        .ok_or_else(|| ExecError::usage("usage: rex verify RECEIPT.json [--public-key BASE64]"))?;
    let raw =
        std::fs::read(path).map_err(|e| ExecError::usage(format!("cannot read {path}: {e}")))?;
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| ExecError::usage(format!("not valid JSON: {e}")))?;
    match verify_receipt(value, public_key) {
        Ok(report) => {
            println!(
                "valid: run {} ended as '{}', signed by {}",
                report.run_id, report.status, report.public_key
            );
            Ok(0)
        }
        Err(e) => {
            eprintln!("rex: INVALID receipt: {e}");
            Ok(3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn positional_task_joins() {
        let o = parse_exec(&args(&["write", "hello.py"])).unwrap();
        assert_eq!(o.task, "write hello.py");
    }

    #[test]
    fn task_flag_wins_over_positional() {
        let o = parse_exec(&args(&["--task", "do x", "ignored"])).unwrap();
        assert_eq!(o.task, "do x");
    }

    #[test]
    fn flags_parse() {
        let o = parse_exec(&args(&[
            "--provider",
            "openai",
            "--model",
            "gpt-x",
            "--max-steps",
            "10",
            "--max-tool-calls",
            "20",
            "--max-tokens",
            "3000",
            "--timeout-secs",
            "120",
            "--json",
            "--yes",
            "--dry-run",
            "--workspace",
            "/tmp/w",
            "--task",
            "t",
        ]))
        .unwrap();
        assert_eq!(o.provider, "openai");
        assert_eq!(o.model.as_deref(), Some("gpt-x"));
        assert_eq!(o.max_steps, Some(10));
        assert_eq!(o.max_tool_calls, Some(20));
        assert_eq!(o.max_tokens, Some(3000));
        assert_eq!(o.timeout_secs, Some(120));
        assert!(o.json && o.yes && o.dry_run);
        assert_eq!(o.workspace.unwrap().to_str().unwrap(), "/tmp/w");
    }

    #[test]
    fn unknown_flag_errors() {
        assert!(parse_exec(&args(&["--frobnicate"])).is_err());
    }

    #[test]
    fn bad_number_errors() {
        assert!(parse_exec(&args(&["--max-steps", "many"])).is_err());
    }

    #[test]
    fn missing_value_errors() {
        assert!(parse_exec(&args(&["--task"])).is_err());
    }
}
