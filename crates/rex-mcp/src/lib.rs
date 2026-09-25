//! MCP 2025-11-25 stdio adapter for the REX Harness daemon.
//!
//! One JSON-RPC object per line. Notifications produce no response. REX
//! exposes only ordinary MCP tools; it does not use sampling or draft Tasks.

pub mod client;

use rex_daemon::HarnessDaemon;
use rex_protocol::{ErrorCode, ProtocolError, ToolName, PROTOCOL_VERSION};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

pub struct McpServer {
    daemon: HarnessDaemon,
    initialized: bool,
}

impl McpServer {
    pub fn new(daemon: HarnessDaemon) -> Self {
        Self {
            daemon,
            initialized: false,
        }
    }

    /// Serve newline-delimited JSON-RPC until EOF. A bad message yields a
    /// JSON-RPC error and does not crash the process.
    pub fn serve<R: BufRead, W: Write>(&mut self, reader: R, mut writer: W) -> std::io::Result<()> {
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(v) => self.handle(v),
                Err(e) => Some(rpc_error(
                    Value::Null,
                    -32700,
                    format!("parse error: {e}"),
                    None,
                )),
            };
            if let Some(response) = response {
                serde_json::to_writer(&mut writer, &response)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
        }
        Ok(())
    }

    pub fn handle(&mut self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        // JSON-RPC notifications have no id and never get a response.
        let notify = id.is_none();
        let id = id.unwrap_or(Value::Null);
        if !notify && !id.is_string() && !id.is_i64() && !id.is_u64() {
            return Some(rpc_error(Value::Null, -32600, "invalid JSON-RPC id", None));
        }
        if request.get("jsonrpc") != Some(&Value::String("2.0".into())) {
            return if notify {
                None
            } else {
                Some(rpc_error(id, -32600, "invalid JSON-RPC envelope", None))
            };
        }
        // Notifications cannot request a reply. Never dispatch tools or
        // initialize through one: otherwise a no-id tools/call could mutate
        // custody without giving the host a receipt or error to handle.
        if notify {
            return None;
        }
        let out = match method {
            "initialize" => {
                self.initialize(&id, request.get("params").cloned().unwrap_or(json!({})))
            }
            "notifications/initialized" => {
                // This is a client acknowledgement, not an alternate handshake.
                // A premature notification must not unlock the tool surface.
                return None;
            }
            "ping" => Ok(json!({})),
            "tools/list" => self
                .require_initialized()
                .map(|_| json!({"tools": tool_descriptors()})),
            "tools/call" => self.require_initialized().and_then(|_| {
                let p = request.get("params").cloned().unwrap_or(json!({}));
                let name = p.get("name").and_then(Value::as_str).ok_or_else(|| {
                    ProtocolError::new(ErrorCode::MalformedRequest, "tool name required")
                })?;
                let tool = ToolName::from_wire_name(name).ok_or_else(|| {
                    ProtocolError::new(ErrorCode::UnknownTool, format!("unknown tool: {name}"))
                })?;
                let args = p.get("arguments").cloned().unwrap_or(json!({}));
                self.daemon.dispatch(tool, args).map(tool_result)
            }),
            _ => {
                return if notify {
                    None
                } else {
                    Some(rpc_error(id, -32601, "method not found", None))
                }
            }
        };
        if notify {
            return None;
        }
        Some(match out {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(e) => protocol_rpc_error(id, e),
        })
    }

    fn initialize(&mut self, _: &Value, params: Value) -> Result<Value, ProtocolError> {
        let offered = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        if offered != MCP_PROTOCOL_VERSION {
            return Err(ProtocolError::new(
                ErrorCode::VersionMismatch,
                format!("unsupported MCP protocol {offered:?}; expected {MCP_PROTOCOL_VERSION}"),
            ));
        }
        self.initialized = true;
        Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "rex-mcp", "version": env!("CARGO_PKG_VERSION") },
            "instructions": format!("REX Harness protocol {PROTOCOL_VERSION}. Caller-driven custody; call rex_execute, then rex_next/tools, and rex_submit. Continuation is cooperative.")
        }))
    }
    fn require_initialized(&self) -> Result<(), ProtocolError> {
        if self.initialized {
            Ok(())
        } else {
            Err(ProtocolError::new(
                ErrorCode::Unauthorized,
                "initialize before calling tools",
            ))
        }
    }
}

fn tool_result(v: Value) -> Value {
    json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into())}],
        "structuredContent":v,"isError":false})
}
fn protocol_rpc_error(id: Value, e: ProtocolError) -> Value {
    // Invalid/unknown method arguments use JSON-RPC codes. Domain failures
    // remain structured data so MCP clients don't erase the REX error code.
    let code = match e.code {
        ErrorCode::UnknownTool => -32601,
        ErrorCode::MalformedRequest => -32602,
        _ => -32000,
    };
    rpc_error(
        id,
        code,
        e.message,
        Some(json!({"code":e.code,"task_id":e.task_id})),
    )
}
fn rpc_error(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut err = json!({"code":code,"message":message.into()});
    if let Some(data) = data {
        err["data"] = data;
    }
    json!({"jsonrpc":"2.0","id":id,"error":err})
}

/// Deliberately explicit schemas: MCP hosts can validate before invoking.
/// No credentials, model settings or provider fields are accepted.
pub fn tool_descriptors() -> Vec<Value> {
    let obj = || json!({"type":"object","additionalProperties":false});
    let mut out = Vec::new();
    for t in ToolName::all() {
        let (description, mut schema) = match t {
            ToolName::Execute => ("Start or resume a durable REX task; plan is frozen at creation.", json!({
                "type":"object","required":["request_id","task","host","operator_is_agent"],
                "properties":{"request_id":{"type":"string"},"task":{"type":"string"},"task_id":{"type":"string"},"resume_handle":{"type":"string"},"follow_up":{"type":"string"},
                "host":{"enum":["human","claude_code","codex","open_code","hermes","antigravity","generic_agent"]},"operator_is_agent":{"type":"boolean"},"ultra":{"type":"boolean"},
                "budgets":{"type":"object"},"proof":{"type":"string"},"plan":{"type":"array","items":{"type":"object","required":["instructions"],"properties":{"instructions":{"type":"string"},"acceptance":{"type":"string"}}}}},"additionalProperties":false})),
            ToolName::Next => ("Heartbeat and get the currently open action.", task_epoch_schema()),
            ToolName::Read => ("Read a file inside the task workspace.", extend(task_epoch_schema(), json!({"path":{"type":"string"},"byte_range":{"type":"array","items":{"type":"integer"},"minItems":2,"maxItems":2}}), &["path"])),
            ToolName::Edit => ("Create or exact-replace a file inside the approved task workspace.", extend(task_epoch_schema(), json!({"path":{"type":"string"},"expected":{"type":"string"},"replacement":{"type":"string"},"create":{"type":"boolean"}}), &["path","replacement"])),
            ToolName::Search => ("Search text inside the task workspace.", extend(task_epoch_schema(), json!({"query":{"type":"string"},"max_results":{"type":"integer"}}), &["query"])),
            ToolName::Run => ("Run one policy-allowed command in the task workspace.", extend(task_epoch_schema(), json!({"argv":{"type":"array","items":{"type":"string"}},"timeout_ms":{"type":"integer"}}), &["argv"])),
            ToolName::Test => ("Run a named allowlisted test recipe and record evidence.", extend(task_epoch_schema(), json!({"recipe":{"enum":["cargo-test","npm-test"]}}), &["recipe"])),
            ToolName::Submit => ("Submit the open action with evidence; REX verifies completion.", extend(task_epoch_schema(), json!({"action_id":{"type":"string"},"narrative":{"type":"string"},"evidence":{"type":"object","additionalProperties":{"type":"string"}}}), &["action_id","narrative"])),
            ToolName::Status => ("Get durable task state.", task_ref_schema()),
            ToolName::Events => ("Read the append-only task event stream.", extend(task_ref_schema(), json!({"after_seq":{"type":"integer"},"limit":{"type":"integer"}}), &[])),
            ToolName::Result => ("Read a terminal result and proof bundle.", task_ref_schema()),
            ToolName::Cancel => ("Operator-cancel a non-terminal task; requires the per-task capability.", json!({"type":"object","required":["task_id","capability"],"properties":{"task_id":{"type":"string"},"capability":{"type":"string"},"reason":{"type":"string"}},"additionalProperties":false})),
            ToolName::HumanStop => ("Final human Stop for any task; requires the trusted launcher's human-stop token, terminal in every phase.", json!({"type":"object","required":["task_id","human_token"],"properties":{"task_id":{"type":"string"},"human_token":{"type":"string"},"reason":{"type":"string"}},"additionalProperties":false})),
            ToolName::UltraOpen => ("Open the Ultra external-host loop: fetch the open candidate or adversary/verifier/visual evidence requests. First open requires contract_draft: a host-drafted acceptance contract the daemon parses, freezes and executes itself; later opens must hash-match the frozen contract.", extend(task_epoch_schema(), json!({"contract_draft":{"type":"string"}}), &[])),
            ToolName::UltraPromote => ("Promote the qualified Ultra candidate bundle into the task workspace with verified rollback.", task_epoch_schema()),
            ToolName::Proof => ("Fetch the deterministic per-task proof bundle: frozen plan, kernel state, per-candidate evidence manifests, promotion receipt, hash-chained events, bundle hash and daemon MAC.", task_ref_schema()),
            ToolName::ProofVerify => ("Independently verify the persisted proof bundle: MAC against the daemon-held key, deterministic reassembly from immutable records, and candidate response-hash integrity.", task_ref_schema()),
            ToolName::ArtifactPut => ("Store evidence bytes in the daemon's content-addressed immutable artifact store, bound to this task (optionally a candidate and round); cite the returned digest as evidence. Stored bytes cannot be altered and one digest cannot be reused across candidates or rounds.", extend(task_epoch_schema(), json!({"kind":{"type":"string"},"bytes_base64":{"type":"string"},"candidate_id":{"type":"string"},"round":{"type":"integer"}}), &["kind","bytes_base64"])),
            ToolName::UltraSubmit => ("Submit one Ultra candidate response or one adversary/verifier/visual evidence item. For candidates, request_id and candidate_id are the answered candidate id.", extend(task_epoch_schema(), json!({"kind":{"enum":["candidate","adversary","verifier","visual"]},"request_id":{"type":"string"},"candidate_id":{"type":"string"},"response_hash":{"type":"string"},"content":{"type":"string"}}), &["kind","request_id","candidate_id","response_hash","content"])),
        };
        // All schemas are objects, even the helper-created ones.
        if schema.is_null() {
            schema = obj();
        }
        out.push(json!({"name":t.wire_name(),"description":description,"inputSchema":schema}));
    }
    out
}
fn task_ref_schema() -> Value {
    json!({"type":"object","required":["task_id"],"properties":{"task_id":{"type":"string"}},"additionalProperties":false})
}
fn task_epoch_schema() -> Value {
    json!({"type":"object","required":["task_id","capability","lease_epoch"],"properties":{"task_id":{"type":"string"},"capability":{"type":"string"},"lease_epoch":{"type":"integer"}},"additionalProperties":false})
}
fn extend(mut base: Value, props: Value, required: &[&str]) -> Value {
    if let (Some(dst), Some(src)) = (base["properties"].as_object_mut(), props.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    if let Some(req) = base["required"].as_array_mut() {
        for r in required {
            req.push(json!(r));
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use rex_daemon::DaemonPolicy;
    use tempfile::tempdir;
    fn server() -> (tempfile::TempDir, McpServer) {
        let d = tempdir().unwrap();
        let daemon = HarnessDaemon::open(
            d.path().join("state"),
            DaemonPolicy::conservative(d.path().join("ws")),
        )
        .unwrap();
        (d, McpServer::new(daemon))
    }
    fn rpc(id: i64, method: &str, params: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
    }
    #[test]
    fn negotiate_list_and_execute_over_stdio() {
        let (_d, mut s) = server();
        let input = format!(
            "{}\n{}\n{}\n",
            rpc(
                1,
                "initialize",
                json!({"protocolVersion":MCP_PROTOCOL_VERSION,"clientInfo":{"name":"test","version":"1"}})
            ),
            rpc(2, "tools/list", json!({})),
            rpc(
                3,
                "tools/call",
                json!({"name":"rex_execute","arguments":{"request_id":"r1","task":"inspect","host":"claude_code","operator_is_agent":true}})
            )
        );
        let mut out = Vec::new();
        s.serve(std::io::Cursor::new(input), &mut out).unwrap();
        let rows: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(rows[1]["result"]["tools"].as_array().unwrap().len(), 19);
        assert_eq!(rows[2]["result"]["structuredContent"]["state"], "active");
        let task_id = rows[2]["result"]["structuredContent"]["task_id"]
            .as_str()
            .unwrap();
        let status = s
            .handle(rpc(
                4,
                "tools/call",
                json!({"name":"rex_status","arguments":{"task_id":task_id}}),
            ))
            .unwrap();
        assert_eq!(
            status["result"]["structuredContent"]["operation"],
            "external_host_required"
        );
        assert_eq!(
            status["result"]["structuredContent"]["packet"]["branch_id"],
            "main"
        );
    }
    #[test]
    fn mismatch_and_unknown_tool_are_structured_errors() {
        let (_d, mut s) = server();
        let bad = s
            .handle(rpc(
                1,
                "initialize",
                json!({"protocolVersion":"1900-01-01"}),
            ))
            .unwrap();
        assert_eq!(bad["error"]["data"]["code"], "version_mismatch");
        let ok = s
            .handle(rpc(
                2,
                "initialize",
                json!({"protocolVersion":MCP_PROTOCOL_VERSION}),
            ))
            .unwrap();
        assert!(ok.get("result").is_some());
        let unknown = s
            .handle(rpc(
                3,
                "tools/call",
                json!({"name":"rex_hack","arguments":{}}),
            ))
            .unwrap();
        assert_eq!(unknown["error"]["data"]["code"], "unknown_tool");
    }
    #[test]
    fn invalid_json_rpc_ids_do_not_initialize() {
        let (_d, mut s) = server();
        for id in [json!(null), json!(true), json!([]), json!({"key": 1})] {
            let response = s
                .handle(json!({"jsonrpc":"2.0", "id":id,
                "method":"initialize", "params":{"protocolVersion":MCP_PROTOCOL_VERSION}}))
                .unwrap();
            assert_eq!(response["error"]["code"], -32600);
            assert!(response["id"].is_null());
        }
        let blocked = s.handle(rpc(1, "tools/list", json!({}))).unwrap();
        assert_eq!(blocked["error"]["data"]["code"], "unauthorized");
        for id in [json!(0), json!("a")] {
            let response = s
                .handle(json!({"jsonrpc":"2.0", "id":id,
                "method":"initialize", "params":{"protocolVersion":MCP_PROTOCOL_VERSION}}))
                .unwrap();
            assert_eq!(response["id"], id);
            assert!(response.get("result").is_some());
        }
    }
    #[test]
    fn tool_call_notification_cannot_mutate_custody() {
        let (_d, mut s) = server();
        s.handle(rpc(
            1,
            "initialize",
            json!({"protocolVersion":MCP_PROTOCOL_VERSION}),
        ))
        .unwrap();
        let args = json!({"request_id":"no-id", "task":"discard me",
                          "host":"generic_agent", "operator_is_agent":true});
        assert!(s
            .handle(json!({"jsonrpc":"2.0", "method":"tools/call",
            "params":{"name":"rex_execute", "arguments":args}}))
            .is_none());
        // If the notification had executed, this reuse of request_id with a
        // different task would be an idempotency conflict.
        let result = s
            .handle(rpc(
                2,
                "tools/call",
                json!({"name":"rex_execute",
            "arguments":{"request_id":"no-id", "task":"actually do this",
                         "host":"generic_agent", "operator_is_agent":true}}),
            ))
            .unwrap();
        assert_eq!(result["result"]["structuredContent"]["state"], "active");
    }
    #[test]
    fn premature_initialized_notification_does_not_unlock_tools() {
        let (_d, mut s) = server();
        assert!(s
            .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .is_none());
        let blocked = s.handle(rpc(1, "tools/list", json!({}))).unwrap();
        assert_eq!(blocked["error"]["data"]["code"], "unauthorized");
        let ready = s
            .handle(rpc(
                2,
                "initialize",
                json!({"protocolVersion":MCP_PROTOCOL_VERSION}),
            ))
            .unwrap();
        assert!(ready.get("result").is_some());
        let listed = s.handle(rpc(3, "tools/list", json!({}))).unwrap();
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 19);
    }
    #[test]
    fn notifications_get_no_response() {
        let (_d, mut s) = server();
        assert!(s
            .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .is_none());
    }
}
