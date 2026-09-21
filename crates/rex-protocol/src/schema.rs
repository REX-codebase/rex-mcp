//! Versioned schema primitives shared by task storage and execution kernels.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const PROTOCOL_SCHEMA_VERSION: &str = "1.1";
pub const TASK_SCHEMA_VERSION: u16 = 2;
pub const KERNEL_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaVersions {
    pub protocol: String,
    pub task: u16,
    pub kernel: u16,
}

impl Default for SchemaVersions {
    fn default() -> Self {
        Self {
            protocol: PROTOCOL_SCHEMA_VERSION.into(),
            task: TASK_SCHEMA_VERSION,
            kernel: KERNEL_SCHEMA_VERSION,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionedRecord<T> {
    pub schema: SchemaVersions,
    pub value: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationEnvelope {
    pub kind: String,
    pub source_version: u16,
    pub target_version: u16,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    InvalidEncoding(String),
    UnsupportedVersion { kind: String, version: u16 },
    TargetMismatch { expected: u16, found: u16 },
    TargetExists,
    Io(String),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MigrationError {}

/// Serialize JSON with recursively sorted object keys. This makes hashes
/// independent of map insertion order while retaining the typed wire shape.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let value = serde_json::to_value(value)?;
    let value = canonical_value(value);
    serde_json::to_vec(&value)
}

pub fn canonical_hash<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let bytes = canonical_json(value)?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn decode_record<T: DeserializeOwned>(
    bytes: &[u8],
    expected_task_version: u16,
) -> Result<T, MigrationError> {
    let record: VersionedRecord<T> = serde_json::from_slice(bytes)
        .map_err(|error| MigrationError::InvalidEncoding(error.to_string()))?;
    if record.schema.task != expected_task_version {
        return Err(MigrationError::TargetMismatch {
            expected: expected_task_version,
            found: record.schema.task,
        });
    }
    Ok(record.value)
}

/// Decode only a known v2 record. Legacy bytes are never overwritten or
/// silently reinterpreted; callers can migrate them into a new file.
pub fn migrate_v1_bytes(bytes: &[u8], kind: &str) -> Result<MigrationEnvelope, MigrationError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| MigrationError::InvalidEncoding(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| MigrationError::InvalidEncoding("record is not an object".into()))?;
    let version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| MigrationError::InvalidEncoding("legacy schema_version is missing".into()))?
        as u16;
    if version != 1 {
        return Err(MigrationError::UnsupportedVersion {
            kind: kind.into(),
            version,
        });
    }
    Ok(MigrationEnvelope {
        kind: kind.into(),
        source_version: 1,
        target_version: 2,
        payload: canonical_json(&value)
            .map_err(|error| MigrationError::InvalidEncoding(error.to_string()))?,
    })
}

pub fn migrate_legacy_file(
    legacy: &std::path::Path,
    target: &std::path::Path,
    kind: &str,
) -> Result<(), MigrationError> {
    if target.exists() {
        return Err(MigrationError::TargetExists);
    }
    let envelope = migrate_v1_bytes(
        &std::fs::read(legacy).map_err(|error| MigrationError::Io(error.to_string()))?,
        kind,
    )?;
    let temporary = target.with_extension("v2.tmp");
    if temporary.exists() {
        return Err(MigrationError::TargetExists);
    }
    let bytes = canonical_json(&envelope)
        .map_err(|error| MigrationError::InvalidEncoding(error.to_string()))?;
    std::fs::write(&temporary, bytes).map_err(|error| MigrationError::Io(error.to_string()))?;
    if let Err(error) = std::fs::rename(&temporary, target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(MigrationError::Io(error.to_string()));
    }
    Ok(())
}

fn canonical_value(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<String, Value> = object
                .into_iter()
                .map(|(key, value)| (key, canonical_value(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_value).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hash_ignores_object_order() {
        let a: Value = serde_json::json!({"b": 2, "a": {"d": 4, "c": 3}});
        let b: Value = serde_json::json!({"a": {"c": 3, "d": 4}, "b": 2});
        assert_eq!(canonical_hash(&a).unwrap(), canonical_hash(&b).unwrap());
    }

    #[test]
    fn task_and_kernel_versions_are_separate() {
        assert_ne!(TASK_SCHEMA_VERSION, 0);
        assert_eq!(
            VersionedRecord::<Value> {
                schema: SchemaVersions::default(),
                value: Value::Null
            }
            .schema
            .kernel,
            2
        );
    }

    #[test]
    fn unknown_legacy_version_fails_closed() {
        let error = migrate_v1_bytes(br#"{"schema_version":9}"#, "task").unwrap_err();
        assert!(matches!(
            error,
            MigrationError::UnsupportedVersion { version: 9, .. }
        ));
    }

    #[test]
    fn disk_migration_preserves_legacy_and_refuses_existing_target() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("state.json");
        let target = directory.path().join("state.v2.json");
        let legacy_bytes = br#"{"schema_version":1,"state":"active"}"#;
        std::fs::write(&legacy, legacy_bytes).unwrap();
        migrate_legacy_file(&legacy, &target, "task").unwrap();
        assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
        assert_eq!(
            migrate_legacy_file(&legacy, &target, "task"),
            Err(MigrationError::TargetExists)
        );
    }
}
