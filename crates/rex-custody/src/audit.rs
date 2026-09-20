//! Append-only, hash-chained custody audit trail.
//!
//! Every transition, heartbeat, tool decision, claim and release appends
//! one event. Each event commits to its predecessor's hash, so truncation
//! or reordering is detectable on load. The chain is verified before the
//! registry trusts recovered state; a broken chain fails closed.

use crate::capability::hex_sha256;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub seq: u64,
    pub ts_ms: u128,
    pub kind: String,
    pub detail: Value,
    pub prev_hash: String,
    pub hash: String,
}

impl AuditEvent {
    fn compute_hash(seq: u64, ts_ms: u128, kind: &str, detail: &Value, prev_hash: &str) -> String {
        let canon = serde_json::json!({
            "seq": seq,
            "ts_ms": ts_ms,
            "kind": kind,
            "detail": detail,
            "prev_hash": prev_hash,
        });
        hex_sha256(serde_json::to_string(&canon).expect("event serializes").as_bytes())
    }

    pub fn verify(&self) -> bool {
        self.hash
            == Self::compute_hash(self.seq, self.ts_ms, &self.kind, &self.detail, &self.prev_hash)
    }
}

pub struct AuditLog {
    path: PathBuf,
    seq: u64,
    last_hash: String,
}

const GENESIS: &str = "rex-custody-genesis";

impl AuditLog {
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let mut seq = 0u64;
        let mut last_hash = GENESIS.to_string();
        if path.exists() {
            let file = fs::File::open(&path)?;
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let ev: AuditEvent = serde_json::from_str(&line)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                if ev.seq != seq + 1 || ev.prev_hash != last_hash || !ev.verify() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("audit chain broken at seq {}", ev.seq),
                    ));
                }
                seq = ev.seq;
                last_hash = ev.hash;
            }
        }
        Ok(Self { path, seq, last_hash })
    }

    pub fn append(&mut self, ts_ms: u128, kind: &str, detail: Value) -> std::io::Result<AuditEvent> {
        let seq = self.seq + 1;
        let hash = AuditEvent::compute_hash(seq, ts_ms, kind, &detail, &self.last_hash);
        let ev = AuditEvent {
            seq,
            ts_ms,
            kind: kind.to_string(),
            detail,
            prev_hash: self.last_hash.clone(),
            hash,
        };
        let mut line = serde_json::to_string(&ev).expect("event serializes");
        line.push('\n');
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
        self.seq = seq;
        self.last_hash = ev.hash.clone();
        Ok(ev)
    }

    pub fn len(&self) -> u64 {
        self.seq
    }
}

/// Read a whole chain for reporting. Verifies every link.
pub fn read_chain(path: &PathBuf) -> std::io::Result<Vec<AuditEvent>> {
    let mut out = Vec::new();
    if !path.exists() {
        return Ok(out);
    }
    let mut last_hash = GENESIS.to_string();
    let file = fs::File::open(path)?;
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: AuditEvent = serde_json::from_str(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if ev.seq as usize != i + 1 || ev.prev_hash != last_hash || !ev.verify() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("audit chain broken at seq {}", ev.seq),
            ));
        }
        last_hash = ev.hash.clone();
        out.push(ev);
    }
    Ok(out)
}
