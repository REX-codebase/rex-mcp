//! `$REX_STATE_DIR/config.json`: small operator-tunable knobs.
//!
//! Today there is exactly one knob: `max_parallel_runs` (default 2,
//! restrained). Background agents multiply cost and API load silently, so
//! the default stays low and the operator raises it deliberately via
//! `rex config set max_parallel_runs N` — never by accident.
//!
//! A missing file means defaults. A corrupt file warns on stderr and falls
//! back to defaults; `rex config set` rewrites it cleanly. The parallelism
//! gate must never brick the dashboard because of a bad config file.

use crate::exec::ExecError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const MAX_PARALLEL_RUNS: &str = "max_parallel_runs";
pub const DEFAULT_MAX_PARALLEL_RUNS: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub max_parallel_runs: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_parallel_runs: DEFAULT_MAX_PARALLEL_RUNS,
        }
    }
}

pub fn config_path(state_dir: &Path) -> PathBuf {
    state_dir.join("config.json")
}

fn parse(raw: &str) -> Result<Config, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let mut cfg = Config::default();
    if let Some(n) = v.get(MAX_PARALLEL_RUNS) {
        cfg.max_parallel_runs = n
            .as_u64()
            .filter(|&n| n >= 1)
            .map(|n| n as usize)
            .ok_or_else(|| format!("{MAX_PARALLEL_RUNS} must be a positive integer"))?;
    }
    Ok(cfg)
}

pub fn load(state_dir: &Path) -> Config {
    let raw = match std::fs::read_to_string(config_path(state_dir)) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            eprintln!("rex: warning: cannot read config: {e}; using defaults");
            return Config::default();
        }
    };
    match parse(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "rex: warning: corrupt config.json ({e}); using defaults — `rex config set` rewrites it cleanly"
            );
            Config::default()
        }
    }
}

pub fn save(state_dir: &Path, cfg: &Config) -> Result<(), ExecError> {
    let path = config_path(state_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ExecError::internal(format!("cannot create state dir: {e}")))?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({ MAX_PARALLEL_RUNS: cfg.max_parallel_runs })).unwrap(),
    )
    .map_err(|e| ExecError::internal(format!("cannot write {}: {e}", path.display())))
}

fn to_json(cfg: &Config) -> Value {
    json!({ MAX_PARALLEL_RUNS: cfg.max_parallel_runs })
}

fn known_key(key: &str) -> bool {
    key == MAX_PARALLEL_RUNS
}

/// `rex config` — show all knobs.
/// `rex config get KEY` — print one knob.
/// `rex config set KEY VALUE` — validate and persist one knob.
pub fn run_config(args: &[String]) -> Result<i32, ExecError> {
    let usage_msg = "usage: rex config [get KEY | set KEY VALUE]";
    let state = crate::exec::state_dir();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            println!(
                "{}",
                serde_json::to_string_pretty(&to_json(&load(&state))).unwrap()
            );
            Ok(0)
        }
        ["get", key] => {
            if !known_key(key) {
                return Err(ExecError::usage(format!(
                    "unknown config key '{key}': known keys: {MAX_PARALLEL_RUNS}"
                )));
            }
            let cfg = load(&state);
            println!("{}", cfg.max_parallel_runs);
            Ok(0)
        }
        ["set", key, value] => {
            if !known_key(key) {
                return Err(ExecError::usage(format!(
                    "unknown config key '{key}': known keys: {MAX_PARALLEL_RUNS}"
                )));
            }
            let n: usize = value.parse().unwrap_or(0);
            if n < 1 {
                return Err(ExecError::usage(format!(
                    "{MAX_PARALLEL_RUNS} must be a positive integer, got '{value}'"
                )));
            }
            let cfg = Config {
                max_parallel_runs: n,
            };
            save(&state, &cfg)?;
            eprintln!("rex: {MAX_PARALLEL_RUNS} = {n}");
            Ok(0)
        }
        _ => Err(ExecError::usage(usage_msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static CONFIG_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fake_state(tag: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("rex-config-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn with_state_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = CONFIG_ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("REX_STATE_DIR");
        std::env::set_var("REX_STATE_DIR", dir);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("REX_STATE_DIR", v),
            None => std::env::remove_var("REX_STATE_DIR"),
        }
        out
    }

    #[test]
    fn defaults_when_missing() {
        let state = fake_state("missing");
        assert_eq!(load(&state), Config::default());
        assert_eq!(Config::default().max_parallel_runs, 2);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn set_get_roundtrip() {
        let state = fake_state("roundtrip");
        save(
            &state,
            &Config {
                max_parallel_runs: 5,
            },
        )
        .unwrap();
        assert_eq!(load(&state).max_parallel_runs, 5);
        with_state_dir(&state, || {
            assert_eq!(
                run_config(&["get".to_string(), MAX_PARALLEL_RUNS.to_string()]).unwrap(),
                0
            );
            assert_eq!(
                run_config(&[
                    "set".to_string(),
                    MAX_PARALLEL_RUNS.to_string(),
                    "3".to_string()
                ])
                .unwrap(),
                0
            );
        });
        assert_eq!(load(&state).max_parallel_runs, 3);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn rejects_bad_values_and_unknown_keys() {
        let state = fake_state("bad");
        with_state_dir(&state, || {
            for bad in ["0", "-1", "many", "2.5"] {
                let e = run_config(&[
                    "set".to_string(),
                    MAX_PARALLEL_RUNS.to_string(),
                    bad.to_string(),
                ])
                .unwrap_err();
                assert_eq!(e.code, 2, "expected usage error for '{bad}'");
            }
            let e = run_config(&["get".to_string(), "frobnicate".to_string()]).unwrap_err();
            assert_eq!(e.code, 2);
            let e = run_config(&["set".to_string(), "frobnicate".to_string(), "1".to_string()])
                .unwrap_err();
            assert_eq!(e.code, 2);
        });
        // Nothing was persisted by the rejected sets.
        assert_eq!(load(&state), Config::default());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let state = fake_state("corrupt");
        std::fs::write(config_path(&state), "{not json").unwrap();
        assert_eq!(load(&state), Config::default());
        std::fs::write(config_path(&state), r#"{"max_parallel_runs": 0}"#).unwrap();
        assert_eq!(load(&state), Config::default());
        let _ = std::fs::remove_dir_all(&state);
    }
}
