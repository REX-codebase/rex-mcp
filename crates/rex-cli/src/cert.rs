//! Signed run certificates (leapfrog bet 1).
//!
//! Every `rex exec --json` receipt carries an Ed25519 certificate over the
//! canonical receipt bytes. Anyone with the receipt — and optionally the
//! expected public key — can check offline that the receipt is exactly what
//! the machine that ran it produced. No REX installation needed to verify.
//!
//! Key custody: one machine keypair at `$REX_STATE_DIR/signing-key.json`
//! (0600 on unix). `rex keygen` rotates it; receipts name their signer by
//! public key, so rotation never invalidates old receipts.

use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519},
};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const ALG: &str = "Ed25519";

#[derive(Debug)]
pub struct CertError(pub String);

impl CertError {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for CertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub fn key_path(state_dir: &Path) -> PathBuf {
    state_dir.join("signing-key.json")
}

fn write_private(path: &Path, pkcs8_b64: &str) -> Result<(), CertError> {
    let body = serde_json::json!({"pkcs8": pkcs8_b64});
    std::fs::write(path, serde_json::to_vec_pretty(&body).unwrap())
        .map_err(|e| CertError::new(format!("cannot write key file: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CertError::new(format!("cannot chmod key file: {e}")))?;
    }
    Ok(())
}

/// Generate a fresh keypair. Refuses to overwrite unless `force`.
pub fn keygen(state_dir: &Path, force: bool) -> Result<String, CertError> {
    let path = key_path(state_dir);
    if path.exists() && !force {
        return Err(CertError::new(format!(
            "signing key already exists at {}; use --force to rotate",
            path.display()
        )));
    }
    std::fs::create_dir_all(state_dir)
        .map_err(|e| CertError::new(format!("cannot create state dir: {e}")))?;
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| CertError::new("key generation failed"))?;
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| CertError::new("key generation failed"))?;
    write_private(
        &path,
        &base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref()),
    )?;
    Ok(base64::engine::general_purpose::STANDARD.encode(key.public_key().as_ref()))
}

fn load_keypair(state_dir: &Path) -> Result<Ed25519KeyPair, CertError> {
    let path = key_path(state_dir);
    let raw =
        std::fs::read(&path).map_err(|e| CertError::new(format!("cannot read key file: {e}")))?;
    let v: Value =
        serde_json::from_slice(&raw).map_err(|_| CertError::new("key file is corrupt"))?;
    let b64 = v
        .get("pkcs8")
        .and_then(Value::as_str)
        .ok_or_else(|| CertError::new("key file is corrupt"))?;
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| CertError::new("key file is corrupt"))?;
    // Copy out of the parsed buffer so the keypair owns its material.
    Ed25519KeyPair::from_pkcs8(&der).map_err(|_| CertError::new("key file is corrupt"))
}

/// Load the machine keypair, generating one on first use.
pub fn load_or_generate(state_dir: &Path) -> Result<Ed25519KeyPair, CertError> {
    if !key_path(state_dir).exists() {
        keygen(state_dir, false)?;
    }
    load_keypair(state_dir)
}

/// Canonical JSON: object keys sorted recursively, no whitespace.
/// Signatures are computed over these bytes so key order can never
/// invalidate a certificate.
pub fn canonical_bytes(v: &Value) -> Vec<u8> {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut out = Map::with_capacity(m.len());
                for k in keys {
                    out.insert(k.clone(), sort(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            _ => value.clone(),
        }
    }
    serde_json::to_vec(&sort(v)).expect("canonical serialization cannot fail")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sign `receipt` (a JSON object) and insert the certificate.
/// Returns the signer's public key (base64).
pub fn sign_receipt(
    key: &Ed25519KeyPair,
    receipt: &mut Map<String, Value>,
) -> Result<String, CertError> {
    let msg = canonical_bytes(&Value::Object(receipt.clone()));
    let digest = ring::digest::digest(&ring::digest::SHA256, &msg);
    let sig = key.sign(&msg);
    let public_b64 = base64::engine::general_purpose::STANDARD.encode(key.public_key().as_ref());
    receipt.insert(
        "certificate".to_string(),
        serde_json::json!({
            "alg": ALG,
            "public_key": public_b64,
            "signature": base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
            "sha256": hex(digest.as_ref()),
        }),
    );
    Ok(public_b64)
}

#[derive(Debug)]
pub struct VerifyReport {
    pub run_id: String,
    pub status: String,
    pub public_key: String,
}

/// Verify a receipt value. `expected_pubkey` (base64), when given, must
/// match the certificate's signer — it pins the receipt to a machine.
pub fn verify_receipt(
    mut v: Value,
    expected_pubkey: Option<&str>,
) -> Result<VerifyReport, CertError> {
    let obj = v
        .as_object_mut()
        .ok_or_else(|| CertError::new("receipt is not a JSON object"))?;
    let cert = obj
        .remove("certificate")
        .ok_or_else(|| CertError::new("receipt has no certificate"))?;
    if cert.get("alg").and_then(Value::as_str) != Some(ALG) {
        return Err(CertError::new("unsupported certificate algorithm"));
    }
    let public_b64 = cert
        .get("public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| CertError::new("certificate is malformed"))?;
    if let Some(expected) = expected_pubkey {
        if expected != public_b64 {
            return Err(CertError::new(
                "certificate signer does not match the expected public key",
            ));
        }
    }
    let sig_b64 = cert
        .get("signature")
        .and_then(Value::as_str)
        .ok_or_else(|| CertError::new("certificate is malformed"))?;
    let sha = cert
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| CertError::new("certificate is malformed"))?;

    let msg = canonical_bytes(&Value::Object(obj.clone()));
    let digest = ring::digest::digest(&ring::digest::SHA256, &msg);
    if hex(digest.as_ref()) != sha {
        return Err(CertError::new(
            "receipt bytes do not match the certificate digest: tampered",
        ));
    }
    let public = base64::engine::general_purpose::STANDARD
        .decode(public_b64)
        .map_err(|_| CertError::new("certificate public key is corrupt"))?;
    let sig = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .map_err(|_| CertError::new("certificate signature is corrupt"))?;
    UnparsedPublicKey::new(&ED25519, public)
        .verify(&msg, &sig)
        .map_err(|_| CertError::new("signature invalid: tampered"))?;

    Ok(VerifyReport {
        run_id: obj
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        status: obj
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        public_key: public_b64.to_string(),
    })
}

// `base64` 0.22 needs the Engine trait in scope for encode/decode.
use base64::Engine as _;

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_state() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "rex-cert-test-{}-{}-{:?}",
            std::process::id(),
            n,
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample_receipt() -> Map<String, Value> {
        serde_json::json!({
            "schema": "rex.exec.receipt/1",
            "run_id": "agent-1-abc",
            "status": "completed",
            "result": "did the thing",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn roundtrip_sign_verify() {
        let state = temp_state();
        let key = load_or_generate(&state).unwrap();
        let mut r = sample_receipt();
        let pubkey = sign_receipt(&key, &mut r).unwrap();
        let report = verify_receipt(Value::Object(r), Some(&pubkey)).unwrap();
        assert_eq!(report.run_id, "agent-1-abc");
        assert_eq!(report.status, "completed");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn tampered_receipt_fails() {
        let state = temp_state();
        let key = load_or_generate(&state).unwrap();
        let mut r = sample_receipt();
        sign_receipt(&key, &mut r).unwrap();
        r.insert("result".to_string(), Value::String("forged".into()));
        assert!(verify_receipt(Value::Object(r), None).is_err());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn wrong_expected_pubkey_fails() {
        let state = temp_state();
        let key = load_or_generate(&state).unwrap();
        let mut r = sample_receipt();
        sign_receipt(&key, &mut r).unwrap();
        let other = temp_state();
        let other_key = load_or_generate(&other).unwrap();
        let other_pub =
            base64::engine::general_purpose::STANDARD.encode(other_key.public_key().as_ref());
        assert!(verify_receipt(Value::Object(r), Some(&other_pub)).is_err());
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn keygen_refuses_overwrite_without_force() {
        let state = temp_state();
        keygen(&state, false).unwrap();
        assert!(keygen(&state, false).is_err());
        keygen(&state, true).unwrap();
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn canonical_form_is_key_order_independent() {
        let a = serde_json::json!({"z": 1, "a": {"y": 2, "b": 3}});
        let b = serde_json::json!({"a": {"b": 3, "y": 2}, "z": 1});
        assert_eq!(canonical_bytes(&a), canonical_bytes(&b));
    }
}
