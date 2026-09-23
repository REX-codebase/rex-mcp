//! Skills marketplace primitives (leapfrog bet 8).
//!
//! A skill pack is a directory with a `skill.json` manifest (schema
//! `rex.skill/1`) and content files, typically a `SKILL.md` entry point.
//! `rex skill install` verifies every file hash in the manifest before
//! copying anything, writes an install lockfile, and refuses on any
//! mismatch. `rex skill verify` re-checks an installed skill against its
//! lockfile, so tampering after install is detectable.
//!
//! `rex exec --skill NAME` loads installed skills into the run: the entry
//! file's content is prepended to the task as a delimited, attributed
//! block, and the receipt records each skill's name, version and entry
//! hash. Skills are trusted content — a malicious skill pack can inject
//! instructions — so installs are hash-verified, explicit per run, and
//! never auto-loaded.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "skill.json";
pub const LOCK_FILE: &str = ".rex-skill-lock.json";
pub const SCHEMA: &str = "rex.skill/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub schema: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    /// Entry file whose content is injected into the run prompt.
    pub entry: String,
    /// Relative path -> hex sha256.
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillLock {
    pub schema: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub installed_at: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub version: String,
    pub entry_hash: String,
    pub body: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check_rel_path(p: &str) -> Result<(), String> {
    if p.is_empty() || p.starts_with('/') || p.contains("..") || p.contains('\\') {
        return Err(format!("unsafe file path in manifest: '{p}'"));
    }
    Ok(())
}

/// Read and structurally validate a manifest from a source directory.
pub fn read_manifest(dir: &Path) -> Result<SkillManifest, String> {
    let raw = std::fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|e| format!("cannot read {MANIFEST_FILE}: {e}"))?;
    let m: SkillManifest =
        serde_json::from_str(&raw).map_err(|e| format!("malformed {MANIFEST_FILE}: {e}"))?;
    validate_manifest(&m)?;
    Ok(m)
}

pub fn validate_manifest(m: &SkillManifest) -> Result<(), String> {
    if m.schema != SCHEMA {
        return Err(format!(
            "unsupported skill schema '{}', want '{SCHEMA}'",
            m.schema
        ));
    }
    if !valid_name(&m.name) {
        return Err(format!(
            "invalid skill name '{}': lowercase letters, digits and '-' only",
            m.name
        ));
    }
    if m.version.trim().is_empty() || m.version.len() > 32 {
        return Err("skill version must be a non-empty string".to_string());
    }
    if m.files.is_empty() {
        return Err("skill manifest lists no files".to_string());
    }
    for (path, hash) in &m.files {
        check_rel_path(path)?;
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("bad sha256 for '{path}'"));
        }
    }
    check_rel_path(&m.entry)?;
    if !m.files.contains_key(&m.entry) {
        return Err(format!("entry '{}' is not in the files map", m.entry));
    }
    Ok(())
}

/// Verify every manifest file against its hash. Fails closed.
pub fn verify_source(dir: &Path, m: &SkillManifest) -> Result<(), String> {
    for (rel, want) in &m.files {
        let bytes =
            std::fs::read(dir.join(rel)).map_err(|e| format!("missing file '{rel}': {e}"))?;
        let got = sha256_hex(&bytes);
        if got != want.to_lowercase() {
            return Err(format!(
                "hash mismatch for '{rel}': manifest lies or file changed"
            ));
        }
    }
    Ok(())
}

/// Install a skill pack from `src` into the library. Returns the install dir.
pub fn install(state_dir: &Path, src: &Path, force: bool) -> Result<PathBuf, String> {
    let m = read_manifest(src)?;
    verify_source(src, &m)?;
    let lib = state_dir.join("skills");
    let dest = lib.join(&m.name);
    if dest.exists() && !force {
        return Err(format!(
            "skill '{}' is already installed (use --force to replace)",
            m.name
        ));
    }
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("cannot clear old install: {e}"))?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("cannot create install dir: {e}"))?;
    for rel in m.files.keys() {
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create dir for '{rel}': {e}"))?;
        }
        let bytes =
            std::fs::read(src.join(rel)).map_err(|e| format!("cannot read '{rel}': {e}"))?;
        std::fs::write(&target, &bytes).map_err(|e| format!("cannot write '{rel}': {e}"))?;
    }
    let lock = SkillLock {
        schema: SCHEMA.to_string(),
        name: m.name.clone(),
        version: m.version.clone(),
        entry: m.entry.clone(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        files: m.files.clone(),
    };
    std::fs::write(
        dest.join(LOCK_FILE),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .map_err(|e| format!("cannot write lockfile: {e}"))?;
    // Belt and braces: verify the installed copy against the lock.
    verify_installed(&dest)?;
    Ok(dest)
}

fn read_lock(dir: &Path) -> Result<SkillLock, String> {
    let raw = std::fs::read_to_string(dir.join(LOCK_FILE))
        .map_err(|e| format!("cannot read install lock: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("malformed install lock: {e}"))
}

/// Re-verify an installed skill against its lockfile. Fails closed.
pub fn verify_installed(dir: &Path) -> Result<SkillLock, String> {
    let lock = read_lock(dir)?;
    for (rel, want) in &lock.files {
        check_rel_path(rel)?;
        let bytes = std::fs::read(dir.join(rel))
            .map_err(|e| format!("installed file '{rel}' unreadable: {e}"))?;
        if sha256_hex(&bytes) != want.to_lowercase() {
            return Err(format!(
                "installed skill '{}' is TAMPERED: '{rel}' no longer matches its lock",
                lock.name
            ));
        }
    }
    Ok(lock)
}

pub fn library_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("skills")
}

/// Load a skill for a run: verify the install, then read the entry file.
pub fn load(state_dir: &Path, name: &str) -> Result<LoadedSkill, String> {
    if !valid_name(name) {
        return Err(format!("invalid skill name '{name}'"));
    }
    let dir = library_dir(state_dir).join(name);
    if !dir.is_dir() {
        return Err(format!("skill '{name}' is not installed"));
    }
    let lock = verify_installed(&dir).map_err(|e| format!("refusing to load skill: {e}"))?;
    check_rel_path(&lock.entry)?;
    let body = std::fs::read_to_string(dir.join(&lock.entry))
        .map_err(|e| format!("cannot read skill entry '{}': {e}", lock.entry))?;
    let entry_hash = lock
        .files
        .get(&lock.entry)
        .cloned()
        .unwrap_or_else(|| sha256_hex(body.as_bytes()));
    Ok(LoadedSkill {
        name: lock.name,
        version: lock.version,
        entry_hash,
        body,
    })
}

/// Delimited, attributed prompt block for the loaded skills.
pub fn prompt_block(skills: &[LoadedSkill]) -> String {
    skills
        .iter()
        .map(|s| {
            format!(
                "<skill name=\"{}\" version=\"{}\">\n{}\n</skill>",
                s.name, s.version, s.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn to_json(s: &LoadedSkill) -> serde_json::Value {
    serde_json::json!({
        "name": s.name,
        "version": s.version,
        "entry_sha256": s.entry_hash,
    })
}

/// Create a manifest for a skill source directory (the publishing helper).
/// The directory must already have a `skill.json` carrying schema, name,
/// version and entry; `pack` fills in the files map by hashing everything
/// present (except `skill.json` itself) and writes the manifest back.
pub fn pack(src: &Path) -> Result<SkillManifest, String> {
    let raw = std::fs::read_to_string(src.join(MANIFEST_FILE)).map_err(|_| {
        format!(
            "cannot pack: {} has no {MANIFEST_FILE} (need schema/name/version/entry)",
            src.display()
        )
    })?;
    let mut m: SkillManifest =
        serde_json::from_str(&raw).map_err(|e| format!("malformed {MANIFEST_FILE}: {e}"))?;
    let mut files = BTreeMap::new();
    collect_files(src, src, &mut files)?;
    if files.is_empty() {
        return Err("nothing to pack: no files besides skill.json".to_string());
    }
    m.files = files;
    validate_manifest(&m)?;
    let out = serde_json::to_string_pretty(&m).unwrap();
    std::fs::write(src.join(MANIFEST_FILE), out)
        .map_err(|e| format!("cannot write {MANIFEST_FILE}: {e}"))?;
    Ok(m)
}

fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for e in entries {
        let e = e.map_err(|e| format!("cannot read dir entry: {e}"))?;
        let p = e.path();
        let rel = p
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == MANIFEST_FILE || rel == LOCK_FILE {
            continue;
        }
        if p.is_dir() {
            collect_files(root, &p, out)?;
        } else {
            let bytes =
                std::fs::read(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            out.insert(rel, sha256_hex(&bytes));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_src(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rex-skill-src-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# sample\nBe terse.\n").unwrap();
        std::fs::write(dir.join("ref.md"), "reference\n").unwrap();
        dir
    }

    fn write_manifest(dir: &Path, files: &BTreeMap<String, String>) {
        let m = SkillManifest {
            schema: SCHEMA.into(),
            name: "sample-skill".into(),
            version: "0.1.0".into(),
            author: "rex".into(),
            description: "a test skill".into(),
            entry: "SKILL.md".into(),
            files: files.clone(),
        };
        std::fs::write(
            dir.join(MANIFEST_FILE),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
    }

    fn hashes(dir: &Path) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for rel in ["SKILL.md", "ref.md"] {
            out.insert(
                rel.to_string(),
                sha256_hex(&std::fs::read(dir.join(rel)).unwrap()),
            );
        }
        out
    }

    #[test]
    fn install_and_load_roundtrip() {
        let src = sample_src("rt");
        write_manifest(&src, &hashes(&src));
        let state = std::env::temp_dir().join("rex-skill-state-rt");
        let _ = std::fs::remove_dir_all(&state);
        let dest = install(&state, &src, false).unwrap();
        assert!(dest.join(LOCK_FILE).exists());
        let loaded = load(&state, "sample-skill").unwrap();
        assert_eq!(loaded.version, "0.1.0");
        assert!(loaded.body.contains("Be terse"));
        let block = prompt_block(&[loaded]);
        assert!(block.contains("<skill name=\"sample-skill\""));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn install_refuses_hash_mismatch() {
        let src = sample_src("mm");
        let h = hashes(&src);
        write_manifest(&src, &h);
        // Tamper after manifesting.
        std::fs::write(src.join("SKILL.md"), "EVIL\n").unwrap();
        let state = std::env::temp_dir().join("rex-skill-state-mm");
        let _ = std::fs::remove_dir_all(&state);
        assert!(install(&state, &src, false).is_err());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn verify_detects_post_install_tamper() {
        let src = sample_src("tm");
        write_manifest(&src, &hashes(&src));
        let state = std::env::temp_dir().join("rex-skill-state-tm");
        let _ = std::fs::remove_dir_all(&state);
        let dest = install(&state, &src, false).unwrap();
        std::fs::write(dest.join("SKILL.md"), "tampered\n").unwrap();
        assert!(verify_installed(&dest).is_err());
        assert!(load(&state, "sample-skill").is_err());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn rejects_path_traversal() {
        let src = sample_src("pt");
        let mut h = hashes(&src);
        h.insert("../evil".to_string(), "0".repeat(64));
        write_manifest(&src, &h);
        let state = std::env::temp_dir().join("rex-skill-state-pt");
        let _ = std::fs::remove_dir_all(&state);
        assert!(install(&state, &src, false).is_err());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn pack_hashes_everything() {
        let src = sample_src("pk");
        std::fs::write(
            src.join(MANIFEST_FILE),
            r#"{"schema":"rex.skill/1","name":"packed","version":"1.0.0","entry":"SKILL.md","files":{}}"#,
        )
        .unwrap();
        let m = pack(&src).unwrap();
        assert_eq!(m.files.len(), 2);
        assert!(verify_source(&src, &m).is_ok());
        let _ = std::fs::remove_dir_all(&src);
    }
}
