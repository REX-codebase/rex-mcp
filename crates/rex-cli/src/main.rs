//! `rex`: the REX headless CLI.
//!
//! Today: `rex exec` (scriptable agent runs for CI), `rex keygen` and
//! `rex verify` (signed run certificates). The desktop app keeps the
//! interactive surface; this binary is the machine surface.

mod cert;
mod exec;

use cert::{keygen, verify_receipt};
use exec::{run_exec, state_dir, ExecError, ExecOptions};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> &'static str {
    "rex: headless REX harness CLI\n\
     \n\
     usage:\n\
     \x20 rex exec [--task TASK | TASK...] [options]\n\
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
     \n\
     credentials: $REX_<PROVIDER>_API_KEY wins; otherwise the file credential\n\
     store under $REX_STATE_DIR (default ~/.rex/harness) is used.\n\
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
