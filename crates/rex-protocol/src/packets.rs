//! Typed protocol-1.1 operation packets shared by daemon and MCP.

use crate::schema::{KERNEL_SCHEMA_VERSION, TASK_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum OperationStatus {
    #[default]
    Queued,
    Prepared,
    Committed,
    Aborted,
    Revoked,
    Stale,
    Conflict,
    ExternalHostRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PacketIdentity {
    pub protocol_version: String,
    pub task_schema_version: u16,
    pub kernel_schema_version: u16,
    pub branch_id: String,
    pub lease_epoch: u64,
    pub resume_nonce: u64,
    pub idempotency_key: String,
}

impl Default for PacketIdentity {
    fn default() -> Self {
        Self::new("main", 0, 0, "")
    }
}

impl PacketIdentity {
    pub fn new(
        branch_id: impl Into<String>,
        lease_epoch: u64,
        resume_nonce: u64,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: crate::PROTOCOL_VERSION.into(),
            task_schema_version: TASK_SCHEMA_VERSION,
            kernel_schema_version: KERNEL_SCHEMA_VERSION,
            branch_id: branch_id.into(),
            lease_epoch,
            resume_nonce,
            idempotency_key: idempotency_key.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationPacket<T> {
    pub identity: PacketIdentity,
    pub status: OperationStatus,
    pub payload: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip_carries_versions_and_identity() {
        let packet = OperationPacket {
            identity: PacketIdentity::new("main", 4, 9, "req-1"),
            status: OperationStatus::Prepared,
            payload: "work",
        };
        let encoded = serde_json::to_string(&packet).unwrap();
        let decoded: OperationPacket<&str> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, packet);
        assert!(encoded.contains("task_schema_version"));
        assert!(encoded.contains("idempotency_key"));
    }

    #[test]
    fn older_status_payloads_remain_decodable() {
        let json = r#"{"identity":{"protocol_version":"1.1","task_schema_version":2,"kernel_schema_version":2,"branch_id":"main","lease_epoch":1,"resume_nonce":1,"idempotency_key":"k"},"status":"queued","payload":null}"#;
        let packet: OperationPacket<()> = serde_json::from_str(json).unwrap();
        assert_eq!(packet.status, OperationStatus::Queued);
    }
}
