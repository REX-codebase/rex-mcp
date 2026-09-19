//! Phase 2: isolated candidate worlds, independent qualification and strict promotion.
//!
//! Candidates are created from one immutable pre-run state and receive the
//! exact same provider, model and Ultra options. Up to two run concurrently,
//! matching the orchestrator's global resource cap. A terminal `Promoted`
//! claim is necessary but not sufficient: selection re-loads each proof
//! bundle and independently re-executes the acceptance contract against that
//! candidate workspace. Only candidates with a complete, internally
//! consistent bundle can be selected. Manifests and selection use stable
//! hashes and run order, never completion timing.

use crate::adversary::AdversaryReport;
use crate::contract::AcceptanceContract;
use crate::evidence::{sha256_hex, EvidenceStore};
use crate::judge::JudgeReport;
use crate::orchestrator::{UltraOptions, UltraRunService, UltraTerminal};
use crate::verify::{self, VerificationReport};
use rex_providers::http::Transport;
use rex_providers::secrets::SecretStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FORMAT_VERSION: u8 = 1;
const MAX_CANDIDATES: usize = 4;
const PARALLELISM: usize = 2;
const POLL_MS: u64 = 100;
const CANDIDATE_TIMEOUT_MS: u64 = 60 * 60 * 1_000;
const COMPETITION_TIMEOUT_MS: u64 = 2 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolationKind {
    GitWorktree,
    Copy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreState {
    Git {
        head: String,
    },
    Copy {
        backup_dir: PathBuf,
        tree_sha256: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateRecord {
    pub index: usize,
    pub id: String,
    pub workspace: PathBuf,
    pub bundle: PathBuf,
    pub branch: Option<String>,
    pub terminal: Option<UltraTerminal>,
    pub repairs: u8,
    pub tokens: u64,
    pub wall_ms: u64,
    pub proof_files: BTreeMap<String, String>,
    pub independently_verified: bool,
    pub disqualifications: Vec<String>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompetitionOutcome {
    pub format_version: u8,
    pub id: String,
    pub target: PathBuf,
    pub isolation: IsolationKind,
    pub pre_state: PreState,
    pub provider: String,
    pub model: String,
    pub candidate_count: usize,
    pub candidates: Vec<CandidateRecord>,
    pub winner: Option<String>,
    pub selection: Option<String>,
    pub promoted: bool,
    pub cancelled: bool,
}

impl CompetitionOutcome {
    pub fn rollback(&self) -> Result<(), String> {
        match &self.pre_state {
            PreState::Git { head } => {
                git(&self.target, &["reset", "--hard", head])?;
                git(&self.target, &["clean", "-fd"])?;
                Ok(())
            }
            PreState::Copy {
                backup_dir,
                tree_sha256,
            } => {
                if tree_hash(backup_dir)? != *tree_sha256 {
                    return Err("pre-run backup hash mismatch; refusing rollback".into());
                }
                sync_dir(backup_dir, &self.target)
            }
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_git_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--show-toplevel"]).is_ok()
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mut entries: Vec<_> = fs::read_dir(src)
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == ".git"
            || name == ".ultra-competition"
            || name == "target"
            || name == "node_modules"
        {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let to = dst.join(&name);
        if kind.is_symlink() {
            return Err(format!(
                "symlinks are not allowed in copy isolation: {}",
                entry.path().display()
            ));
        }
        if kind.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn hash_tree(root: &Path, prefix: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == ".git"
            || name == ".ultra-competition"
            || name == "target"
            || name == "node_modules"
        {
            continue;
        }
        let path = entry.path();
        let rel = prefix.join(&name).to_string_lossy().replace('\\', "/");
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!(
                "symlinks are not allowed in copy isolation: {}",
                path.display()
            ));
        }
        if kind.is_dir() {
            hash_tree(&path, &prefix.join(name), out)?;
        } else if kind.is_file() {
            out.push((
                rel,
                format!(
                    "{:x}",
                    Sha256::digest(fs::read(path).map_err(|e| e.to_string())?)
                ),
            ));
        }
    }
    Ok(())
}

fn tree_hash(root: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    hash_tree(root, Path::new(""), &mut files)?;
    serde_json::to_vec(&files)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|e| e.to_string())
}

fn sync_dir(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mut wanted = Vec::new();
    hash_tree(src, Path::new(""), &mut wanted)?;
    let wanted: HashSet<String> = wanted.into_iter().map(|(p, _)| p).collect();
    let mut existing = Vec::new();
    hash_tree(dst, Path::new(""), &mut existing)?;
    for (rel, _) in existing {
        if !wanted.contains(&rel) {
            fs::remove_file(dst.join(rel)).map_err(|e| e.to_string())?;
        }
    }
    copy_dir(src, dst)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(tmp, path).map_err(|e| e.to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn bundle_hashes(bundle: &Path) -> Result<BTreeMap<String, String>, String> {
    let required = [
        "contract.json",
        "verification.json",
        "adversary.json",
        "verdicts.json",
        "ledger.jsonl",
        "evidence/manifest.jsonl",
        "phase3.json",
    ];
    let mut hashes = BTreeMap::new();
    for rel in required {
        let bytes =
            fs::read(bundle.join(rel)).map_err(|e| format!("missing proof file {rel}: {e}"))?;
        hashes.insert(rel.to_string(), sha256_hex(&bytes));
    }
    Ok(hashes)
}

fn qualify(record: &mut CandidateRecord) {
    record.disqualifications.clear();
    if record.terminal != Some(UltraTerminal::Promoted) {
        record
            .disqualifications
            .push("Phase 1 pipeline did not promote".into());
        return;
    }
    let hashes = match bundle_hashes(&record.bundle) {
        Ok(v) => v,
        Err(e) => {
            record.disqualifications.push(e);
            return;
        }
    };
    let contract: AcceptanceContract = match read_json(&record.bundle.join("contract.json")) {
        Ok(v) => v,
        Err(e) => {
            record.disqualifications.push(e);
            return;
        }
    };
    let original: VerificationReport = match read_json(&record.bundle.join("verification.json")) {
        Ok(v) => v,
        Err(e) => {
            record.disqualifications.push(e);
            return;
        }
    };
    let adversary: AdversaryReport = match read_json(&record.bundle.join("adversary.json")) {
        Ok(v) => v,
        Err(e) => {
            record.disqualifications.push(e);
            return;
        }
    };
    let phase3: crate::phase3::Phase3Bundle = match read_json(&record.bundle.join("phase3.json")) {
        Ok(v) => v,
        Err(e) => {
            record.disqualifications.push(e);
            return;
        }
    };
    if !phase3.gate.promotable
        || phase3.mutations.survived > 0
        || phase3.mutations.inconclusive > 0
        || phase3.differential.unexplained > 0
    {
        record
            .disqualifications
            .push("Phase 3 counterexample/mutation/differential gate did not pass".into());
    }
    let judge: JudgeReport = match read_json(&record.bundle.join("verdicts.json")) {
        Ok(v) => v,
        Err(e) => {
            record.disqualifications.push(e);
            return;
        }
    };
    if !original.executable_all_proven {
        record
            .disqualifications
            .push("stored verification has failed executable obligations".into());
    }
    if adversary.inconclusive || !adversary.defects.is_empty() {
        record
            .disqualifications
            .push("adversary is inconclusive or has standing defects".into());
    }
    if !judge.all_passed || judge.verdicts.len() != contract.obligations.len() {
        record
            .disqualifications
            .push("judge did not pass every obligation".into());
    }
    let independent_dir = record.bundle.join("selection-verification");
    let mut evidence = match EvidenceStore::open(&independent_dir) {
        Ok(v) => v,
        Err(e) => {
            record
                .disqualifications
                .push(format!("selection verifier cannot open: {e}"));
            return;
        }
    };
    let fresh = verify::verify_contract(&contract, &record.workspace, &mut evidence);
    let _ = write_json(&record.bundle.join("selection-verification.json"), &fresh);
    if !fresh.executable_all_proven {
        record
            .disqualifications
            .push("independent executable verification failed".into());
    }
    record.proof_files = hashes;
    record.independently_verified = record.disqualifications.is_empty();
}

/// Stable selection: qualification, then fewer repairs, fewer tokens, then
/// original candidate order. Wall time is evidence only because scheduler
/// timing is not deterministic.
pub fn select(candidates: &[CandidateRecord]) -> Option<&CandidateRecord> {
    candidates
        .iter()
        .filter(|c| c.independently_verified && c.disqualifications.is_empty())
        .min_by_key(|c| (c.repairs, c.tokens, c.index))
}

pub struct Competition;

impl Competition {
    #[allow(clippy::too_many_arguments)]
    pub fn run<S: SecretStore + 'static, T: Transport + 'static>(
        svc: &UltraRunService<S, T>,
        task: &str,
        provider: &str,
        model: Option<&str>,
        options: UltraOptions,
        target: &Path,
        n: usize,
        dir: &Path,
    ) -> Result<CompetitionOutcome, String> {
        if !(1..=MAX_CANDIDATES).contains(&n) {
            return Err(format!("candidate count must be 1..={MAX_CANDIDATES}"));
        }
        if !target.is_dir() {
            return Err("target must be an existing directory".into());
        }
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let checkpoint = dir.join("candidates.json");
        let requested_model = model.unwrap_or_default().to_string();
        let resume: Option<CompetitionOutcome> = if checkpoint.exists() {
            let prior: CompetitionOutcome = read_json(&checkpoint)?;
            if prior.format_version != FORMAT_VERSION
                || prior.target != target
                || prior.provider != provider
                || prior.model != requested_model
                || prior.candidate_count != n
            {
                return Err("checkpoint does not match this competition request".into());
            }
            if prior.promoted {
                return Ok(prior);
            }
            Some(prior)
        } else {
            None
        };
        let id = resume
            .as_ref()
            .map(|o| o.id.clone())
            .unwrap_or_else(|| format!("comp-{:x}", crate::now_ms()));
        let model = requested_model;
        let (isolation, pre_state) = if let Some(prior) = &resume {
            match &prior.pre_state {
                PreState::Git { head } if git(target, &["rev-parse", "HEAD"])? != *head => {
                    return Err("git target moved since the checkpoint".into());
                }
                PreState::Copy {
                    backup_dir,
                    tree_sha256,
                } if tree_hash(backup_dir)? != *tree_sha256 => {
                    return Err("checkpoint backup hash mismatch".into());
                }
                _ => {}
            }
            (prior.isolation, prior.pre_state.clone())
        } else if is_git_repo(target) {
            if !git(target, &["status", "--porcelain"])?.is_empty() {
                return Err("git target must be clean before candidate isolation".into());
            }
            let head = git(target, &["rev-parse", "HEAD"])?;
            (IsolationKind::GitWorktree, PreState::Git { head })
        } else {
            let backup = dir.join("pre-run-backup");
            copy_dir(target, &backup)?;
            let hash = tree_hash(&backup)?;
            (
                IsolationKind::Copy,
                PreState::Copy {
                    backup_dir: backup,
                    tree_sha256: hash,
                },
            )
        };
        let mut records = resume
            .as_ref()
            .map(|o| o.candidates.clone())
            .unwrap_or_default();
        records.retain(|record| record.done && record.independently_verified);
        let finished: HashSet<usize> = records.iter().map(|record| record.index).collect();
        for index in 0..n {
            if finished.contains(&index) {
                continue;
            }
            let candidate_dir = dir.join(format!("candidate-{index}"));
            let workspace = candidate_dir.join("workspace");
            let _ = fs::remove_dir_all(&candidate_dir);
            fs::create_dir_all(&candidate_dir).map_err(|e| e.to_string())?;
            let branch = match &pre_state {
                PreState::Git { head } => {
                    let branch = format!("ultra/{id}/c{index}");
                    let _ = git(
                        target,
                        &[
                            "worktree",
                            "remove",
                            "--force",
                            workspace.to_string_lossy().as_ref(),
                        ],
                    );
                    let _ = git(target, &["branch", "-D", &branch]);
                    git(
                        target,
                        &[
                            "worktree",
                            "add",
                            "--detach",
                            workspace.to_string_lossy().as_ref(),
                            head,
                        ],
                    )?;
                    git(&workspace, &["checkout", "-b", &branch])?;
                    Some(branch)
                }
                PreState::Copy { .. } => {
                    copy_dir(target, &workspace)?;
                    None
                }
            };
            records.push(CandidateRecord {
                index,
                id: format!("{id}-c{index}"),
                workspace,
                bundle: candidate_dir.join("ultra"),
                branch,
                terminal: None,
                repairs: 0,
                tokens: 0,
                wall_ms: 0,
                proof_files: BTreeMap::new(),
                independently_verified: false,
                disqualifications: Vec::new(),
                done: false,
            });
        }
        records.sort_by_key(|record| record.index);
        let mut outcome = CompetitionOutcome {
            format_version: FORMAT_VERSION,
            id,
            target: target.to_path_buf(),
            isolation,
            pre_state,
            provider: provider.to_string(),
            model: model.clone(),
            candidate_count: n,
            candidates: records.clone(),
            winner: None,
            selection: None,
            promoted: false,
            cancelled: false,
        };
        write_json(&checkpoint, &outcome)?;

        let competition_started = Instant::now();
        let pending: Vec<CandidateRecord> = records
            .iter()
            .filter(|record| !record.done)
            .cloned()
            .collect();
        for batch in pending.chunks(PARALLELISM) {
            if competition_started.elapsed().as_millis() as u64 > COMPETITION_TIMEOUT_MS {
                outcome.cancelled = true;
                break;
            }
            let (tx, rx) = mpsc::channel();
            std::thread::scope(|scope| {
                for seed in batch.iter().cloned() {
                    let tx = tx.clone();
                    let options = options.clone();
                    let model = model.clone();
                    scope.spawn(move || {
                        let started = Instant::now();
                        let mut record = seed;
                        let result = svc.begin_in(
                            &record.id,
                            task,
                            provider,
                            if model.is_empty() { None } else { Some(&model) },
                            options,
                            record.bundle.parent().unwrap_or(dir).to_path_buf(),
                            record.workspace.clone(),
                        );
                        match result {
                            Err(e) => record
                                .disqualifications
                                .push(format!("candidate start failed: {e}")),
                            Ok(_) => loop {
                                std::thread::sleep(Duration::from_millis(POLL_MS));
                                let Some(snapshot) = svc.snapshot(&record.id) else {
                                    record
                                        .disqualifications
                                        .push("candidate run vanished".into());
                                    break;
                                };
                                record.repairs = snapshot.repair;
                                record.tokens = snapshot
                                    .builder
                                    .as_ref()
                                    .map(|b| b.tokens_used)
                                    .unwrap_or(record.tokens);
                                if snapshot
                                    .builder
                                    .as_ref()
                                    .and_then(|b| b.pending_approval.as_ref())
                                    .is_some()
                                {
                                    let _ = svc.decide(&record.id, true);
                                }
                                if let Some(terminal) = snapshot.terminal {
                                    record.terminal = Some(terminal);
                                    break;
                                }
                                if started.elapsed().as_millis() as u64 > CANDIDATE_TIMEOUT_MS {
                                    let _ = svc.cancel(&record.id);
                                    record.terminal = Some(UltraTerminal::Cancelled);
                                    record
                                        .disqualifications
                                        .push("candidate wall-time budget exceeded".into());
                                    break;
                                }
                            },
                        }
                        record.wall_ms = started.elapsed().as_millis() as u64;
                        if record.terminal == Some(UltraTerminal::Promoted) {
                            if let Some(branch) = &record.branch {
                                match git(&record.workspace, &["status", "--porcelain"]) {
                                    Ok(status) if !status.is_empty() => {
                                        let commit = git(&record.workspace, &["add", "-A"])
                                            .and_then(|_| {
                                                git(
                                                    &record.workspace,
                                                    &[
                                                        "-c",
                                                        "user.name=REX-codebase",
                                                        "-c",
                                                        "user.email=aggu000000@gmail.com",
                                                        "commit",
                                                        "-q",
                                                        "-m",
                                                        &format!("Ultra candidate {}", record.id),
                                                    ],
                                                )
                                            });
                                        if let Err(e) = commit {
                                            record.disqualifications.push(e);
                                        }
                                    }
                                    Err(e) => record.disqualifications.push(e),
                                    _ => {}
                                }
                                if git(&record.workspace, &["rev-parse", "--verify", branch])
                                    .is_err()
                                {
                                    record
                                        .disqualifications
                                        .push("candidate branch vanished".into());
                                }
                            }
                            qualify(&mut record);
                        }
                        record.done = true;
                        let _ = tx.send(record);
                    });
                }
            });
            drop(tx);
            for record in rx {
                if let Some(existing) = outcome
                    .candidates
                    .iter_mut()
                    .find(|item| item.index == record.index)
                {
                    *existing = record;
                } else {
                    outcome.candidates.push(record);
                    outcome.candidates.sort_by_key(|item| item.index);
                }
                write_json(&checkpoint, &outcome)?;
            }
        }

        if !outcome.cancelled {
            if let Some(winner) = select(&outcome.candidates).cloned() {
                match outcome.isolation {
                    IsolationKind::GitWorktree => {
                        let branch = winner.branch.as_deref().ok_or("winner missing branch")?;
                        git(
                            target,
                            &[
                                "merge",
                                "--no-ff",
                                "-m",
                                &format!("Ultra promotion: {}", winner.id),
                                branch,
                            ],
                        )?;
                    }
                    IsolationKind::Copy => sync_dir(&winner.workspace, target)?,
                }
                outcome.selection = Some(format!(
                    "{} selected: independently verified; repairs={}; tokens={}; order={} (stable tie-break: repairs, tokens, order)",
                    winner.id, winner.repairs, winner.tokens, winner.index
                ));
                outcome.winner = Some(winner.id);
                outcome.promoted = true;
            } else {
                outcome.selection = Some(
                    "no candidate satisfied every Phase 1 and independent selection obligation"
                        .into(),
                );
            }
        }
        write_json(&checkpoint, &outcome)?;

        // Archive manifests, then remove disposable worktrees and branches.
        if outcome.isolation == IsolationKind::GitWorktree {
            for candidate in &outcome.candidates {
                let _ = git(
                    target,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        candidate.workspace.to_string_lossy().as_ref(),
                    ],
                );
                if let Some(branch) = &candidate.branch {
                    let _ = git(target, &["branch", "-D", branch]);
                }
            }
        } else {
            for candidate in &outcome.candidates {
                let _ = fs::remove_dir_all(&candidate.workspace);
            }
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(index: usize, repairs: u8, tokens: u64, eligible: bool) -> CandidateRecord {
        CandidateRecord {
            index,
            id: format!("c{index}"),
            workspace: PathBuf::new(),
            bundle: PathBuf::new(),
            branch: None,
            terminal: Some(UltraTerminal::Promoted),
            repairs,
            tokens,
            wall_ms: 999 - index as u64,
            proof_files: BTreeMap::new(),
            independently_verified: eligible,
            disqualifications: if eligible {
                vec![]
            } else {
                vec!["failed obligation".into()]
            },
            done: true,
        }
    }

    #[test]
    fn selection_never_chooses_failed_obligations() {
        let candidates = vec![candidate(0, 0, 1, false), candidate(1, 1, 20, true)];
        assert_eq!(select(&candidates).unwrap().id, "c1");
    }

    #[test]
    fn selection_is_stable_and_ignores_wall_clock() {
        let candidates = vec![candidate(0, 0, 10, true), candidate(1, 0, 10, true)];
        assert_eq!(select(&candidates).unwrap().id, "c0");
    }

    #[test]
    fn copy_isolation_promotion_and_rollback_are_exact() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let backup = tmp.path().join("backup");
        let winner = tmp.path().join("winner");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep.txt"), "before").unwrap();
        fs::write(target.join("delete.txt"), "old").unwrap();
        copy_dir(&target, &backup).unwrap();
        copy_dir(&target, &winner).unwrap();
        fs::write(winner.join("keep.txt"), "after").unwrap();
        fs::remove_file(winner.join("delete.txt")).unwrap();
        fs::write(winner.join("new.txt"), "new").unwrap();
        sync_dir(&winner, &target).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("keep.txt")).unwrap(),
            "after"
        );
        assert!(!target.join("delete.txt").exists());
        let outcome = CompetitionOutcome {
            format_version: FORMAT_VERSION,
            id: "test".into(),
            target: target.clone(),
            isolation: IsolationKind::Copy,
            pre_state: PreState::Copy {
                backup_dir: backup.clone(),
                tree_sha256: tree_hash(&backup).unwrap(),
            },
            provider: "gemini".into(),
            model: "pinned".into(),
            candidate_count: 1,
            candidates: vec![],
            winner: None,
            selection: None,
            promoted: false,
            cancelled: false,
        };
        outcome.rollback().unwrap();
        assert_eq!(tree_hash(&target).unwrap(), tree_hash(&backup).unwrap());
    }

    #[test]
    fn git_promotion_and_rollback_restore_exact_head() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q"]).unwrap();
        git(repo, &["config", "user.name", "test"]).unwrap();
        git(repo, &["config", "user.email", "test@example.invalid"]).unwrap();
        fs::write(repo.join("file.txt"), "before").unwrap();
        git(repo, &["add", "."]).unwrap();
        git(repo, &["commit", "-qm", "base"]).unwrap();
        let head = git(repo, &["rev-parse", "HEAD"]).unwrap();
        fs::write(repo.join("file.txt"), "after").unwrap();
        git(repo, &["commit", "-am", "change", "-q"]).unwrap();
        let outcome = CompetitionOutcome {
            format_version: FORMAT_VERSION,
            id: "test".into(),
            target: repo.to_path_buf(),
            isolation: IsolationKind::GitWorktree,
            pre_state: PreState::Git { head: head.clone() },
            provider: "gemini".into(),
            model: "pinned".into(),
            candidate_count: 1,
            candidates: vec![],
            winner: None,
            selection: None,
            promoted: true,
            cancelled: false,
        };
        outcome.rollback().unwrap();
        assert_eq!(git(repo, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(fs::read_to_string(repo.join("file.txt")).unwrap(), "before");
    }

    #[test]
    fn deterministic_tree_hash_ignores_creation_order() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("x"), "1").unwrap();
        fs::write(a.join("y"), "2").unwrap();
        fs::write(b.join("y"), "2").unwrap();
        fs::write(b.join("x"), "1").unwrap();
        assert_eq!(tree_hash(&a).unwrap(), tree_hash(&b).unwrap());
    }
}
