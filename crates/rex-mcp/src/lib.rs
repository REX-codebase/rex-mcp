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
pub const MCP_COMPAT_PROTOCOL_VERSION: &str = "2025-06-18";

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
    pub fn serve<R: BufRead, W: Write>(
        &mut self,
        mut reader: R,
        mut writer: W,
    ) -> std::io::Result<()> {
        // Bound each untrusted stdio frame before allocation, while continuing
        // to serve later requests after an oversized or invalid UTF-8 frame.
        while let Some((bytes, oversized)) = read_frame(&mut reader)? {
            let response = if oversized {
                Some(rpc_error(
                    Value::Null,
                    -32700,
                    "MCP frame exceeds 1 MiB",
                    None,
                ))
            } else if bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            } else {
                match std::str::from_utf8(&bytes) {
                    Ok(line) => match serde_json::from_str::<Value>(line) {
                        Ok(v) => {
                            // A host-supplied token is scoped to this live call only.
                            // Emit before dispatch so a slow gate can display activity,
                            // never after the response or for unrelated requests.
                            if let Some(notification) = progress_start(&v, self.initialized) {
                                serde_json::to_writer(&mut writer, &notification)?;
                                writer.write_all(b"\n")?;
                                writer.flush()?;
                            }
                            self.handle(v)
                        }
                        Err(e) => Some(rpc_error(
                            Value::Null,
                            -32700,
                            format!("parse error: {e}"),
                            None,
                        )),
                    },
                    Err(_) => Some(rpc_error(
                        Value::Null,
                        -32700,
                        "MCP frame is not UTF-8",
                        None,
                    )),
                }
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
        // A malformed request is not a notification. In particular, an array
        // or an object without a string method must receive an error rather
        // than disappearing as if it were a valid no-id notification.
        if !request.is_object()
            || !request
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| !method.is_empty())
        {
            return Some(rpc_error(
                Value::Null,
                -32600,
                "invalid JSON-RPC request",
                None,
            ));
        }
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap();
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
            "resources/list" => self.require_initialized().map(|_| json!({
                "resources": [{
                    "uri": "rex://workflow/quickstart",
                    "name": "REX task workflow quickstart",
                    "description": "How an MCP host uses REX task custody and scoped tools.",
                    "mimeType": "text/plain"
                }]
            })),
            "resources/templates/list" => self.require_initialized().map(|_| json!({
                "resourceTemplates": [{
                    "uriTemplate": "rex://task/{task_id}/status",
                    "name": "REX task status",
                    "description": "Read the current custody status of a known REX task ID.",
                    "mimeType": "application/json"
                }, {
                    "uriTemplate": "rex://task/{task_id}/events/{after_seq}",
                    "name": "REX task events",
                    "description": "Read up to 100 append-only events after a known sequence cursor; start at 0.",
                    "mimeType": "application/json"
                }, {
                    "uriTemplate": "rex://task/{task_id}/result",
                    "name": "REX task result",
                    "description": "Read the terminal result and proof bundle of a known REX task ID.",
                    "mimeType": "application/json"
                }]
            })),
            "resources/read" => self.require_initialized().and_then(|_| {
                let uri = request.get("params").and_then(|p| p.get("uri"))
                    .and_then(Value::as_str).ok_or_else(|| {
                        ProtocolError::new(ErrorCode::MalformedRequest, "resource URI required")
                    })?;
                if let Some(path) = uri.strip_prefix("rex://task/") {
                    if let Some((task_id, after_seq)) = path.split_once("/events/") {
                        if task_id.is_empty() || task_id.contains('/') {
                            return Err(ProtocolError::new(
                                ErrorCode::MalformedRequest,
                                "invalid task events resource URI",
                            ));
                        }
                        let after_seq = after_seq.parse::<u64>().map_err(|_| {
                            ProtocolError::new(
                                ErrorCode::MalformedRequest,
                                "invalid task events cursor",
                            )
                        })?;
                        let events = self.daemon.dispatch(
                            ToolName::Events,
                            json!({"task_id": task_id, "after_seq": after_seq, "limit": 100}),
                        )?;
                        return Ok(json!({"contents": [{
                            "uri": uri,
                            "mimeType": "application/json",
                            "text": serde_json::to_string_pretty(&events).map_err(|e| {
                                ProtocolError::new(
                                    ErrorCode::MalformedRequest,
                                    format!("cannot encode task events: {e}"),
                                )
                            })?
                        }]}));
                    }
                }
                if let Some(task_id) = uri
                    .strip_prefix("rex://task/")
                    .and_then(|path| path.strip_suffix("/result"))
                {
                    if task_id.is_empty() || task_id.contains('/') {
                        return Err(ProtocolError::new(
                            ErrorCode::MalformedRequest,
                            "invalid task result resource URI",
                        ));
                    }
                    let result = self.daemon.dispatch(
                        ToolName::Result,
                        json!({"task_id": task_id}),
                    )?;
                    return Ok(json!({"contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": serde_json::to_string_pretty(&result).map_err(|e| {
                            ProtocolError::new(
                                ErrorCode::MalformedRequest,
                                format!("cannot encode task result: {e}"),
                            )
                        })?
                    }]}));
                }
                if let Some(task_id) = uri
                    .strip_prefix("rex://task/")
                    .and_then(|path| path.strip_suffix("/status"))
                {
                    if task_id.is_empty() || task_id.contains('/') {
                        return Err(ProtocolError::new(
                            ErrorCode::MalformedRequest,
                            "invalid task status resource URI",
                        ));
                    }
                    let status = self.daemon.dispatch(
                        ToolName::Status,
                        json!({"task_id": task_id}),
                    )?;
                    return Ok(json!({"contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": serde_json::to_string_pretty(&status).map_err(|e| {
                            ProtocolError::new(
                                ErrorCode::MalformedRequest,
                                format!("cannot encode task status: {e}"),
                            )
                        })?
                    }]}));
                }
                if uri != "rex://workflow/quickstart" {
                    return Err(ProtocolError::new(
                        ErrorCode::MalformedRequest, format!("unknown resource: {uri}")
                    ));
                }
                Ok(json!({"contents": [{
                    "uri": uri,
                    "mimeType": "text/plain",
                    "text": "Start with rex_execute to create a durable task and follow its returned next action. Use the returned task_id with rex_next and scoped rex_tools, then rex_submit with evidence. Check rex_status to inspect state. Mutations require trusted launcher approval; denied or stale leases stop rather than bypassing custody. The MCP host controls its own continuation and limits; REX does not force further host calls."
                }]}))
            }),
            "prompts/list" => self.require_initialized().map(|_| json!({
                "prompts": [{
                    "name": "rex_task_workflow",
                    "title": "Run a durable REX task",
                    "description": "Guide the host through REX task custody, continuation, and submission.",
                    "arguments": [{
                        "name": "task",
                        "description": "The task to run with durable custody.",
                        "required": true
                    }]
                }]
            })),
            "prompts/get" => self.require_initialized().and_then(|_| {
                let name = request.get("params").and_then(|p| p.get("name")).and_then(Value::as_str).ok_or_else(|| {
                    ProtocolError::new(ErrorCode::MalformedRequest, "prompt name required")
                })?;
                if name != "rex_task_workflow" {
                    return Err(ProtocolError::new(
                        ErrorCode::MalformedRequest, format!("unknown prompt: {name}")
                    ));
                }
                let task = request
                    .get("params")
                    .and_then(|p| p.get("arguments"))
                    .and_then(|a| a.get("task"))
                    .and_then(Value::as_str)
                    .filter(|task| !task.trim().is_empty())
                    .ok_or_else(|| {
                        ProtocolError::new(
                            ErrorCode::MalformedRequest,
                            "non-empty task argument required",
                        )
                    })?;
                Ok(json!({
                    "description": "Use REX to keep a task's plan, tool outcomes, and continuation in durable custody.",
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!("For this task: {task}\n\nCall rex_execute with the task and host. Use the returned task_id with rex_next, rex_tools, and rex_submit as directed. Follow tool responses rather than guessing the next step. The host controls continuation and limits; REX does not force further calls.")
                        }
                    }]
                }))
            }),
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
            Err(e)
                if method == "tools/call"
                    && self.initialized
                    && !matches!(e.code, ErrorCode::MalformedRequest | ErrorCode::UnknownTool) =>
            {
                json!({"jsonrpc":"2.0","id":id,"result":tool_error(e)})
            }
            Err(e) => protocol_rpc_error(id, e),
        })
    }

    fn initialize(&mut self, _: &Value, params: Value) -> Result<Value, ProtocolError> {
        if self.initialized {
            return Err(ProtocolError::new(
                ErrorCode::MalformedRequest,
                "MCP session is already initialized",
            ));
        }
        let offered = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        // Per MCP version negotiation, return the caller's version when
        // supported. Otherwise offer our latest; the client can disconnect.
        // Missing/malformed version is an invalid initialize request.
        if offered.is_empty() {
            return Err(ProtocolError::new(
                ErrorCode::MalformedRequest,
                "protocolVersion required",
            ));
        }
        let version = if offered == MCP_COMPAT_PROTOCOL_VERSION {
            MCP_COMPAT_PROTOCOL_VERSION
        } else {
            MCP_PROTOCOL_VERSION
        };
        self.initialized = true;
        Ok(json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false }, "prompts": { "listChanged": false }, "resources": { "listChanged": false } },
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

fn progress_start(request: &Value, initialized: bool) -> Option<Value> {
    if !initialized
        || request.get("jsonrpc")?.as_str()? != "2.0"
        || request.get("method")?.as_str()? != "tools/call"
        || !request.get("id")?.is_string() && !request.get("id")?.is_number()
    {
        return None;
    }
    let params = request.get("params")?;
    let name = params.get("name")?.as_str()?;
    let long_call = matches!(
        name,
        "rex_execute"
            | "rex_run"
            | "rex_test"
            | "rex_ultra_open"
            | "rex_ultra_submit"
            | "rex_ultra_promote"
            | "rex_proof_verify"
    );
    if !long_call {
        return None;
    }
    let token = params.get("_meta")?.get("progressToken")?;
    if !(token.is_string() || token.is_i64() || token.is_u64()) {
        return None;
    }
    Some(
        json!({"jsonrpc":"2.0","method":"notifications/progress","params":{
            "progressToken":token,"progress":1,"message":"REX processing request"
        }}),
    )
}

fn read_frame<R: BufRead>(reader: &mut R) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    let mut bytes = Vec::new();
    let mut oversized = false;
    let mut seen = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(seen.then_some((bytes, oversized)));
        }
        seen = true;
        let count = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map_or(chunk.len(), |i| i + 1);
        if bytes.len().saturating_add(count) > MAX_FRAME_BYTES {
            oversized = true;
        }
        if !oversized {
            bytes.extend_from_slice(&chunk[..count]);
        }
        let ended = chunk[count - 1] == b'\n';
        reader.consume(count);
        if ended {
            return Ok(Some((bytes, oversized)));
        }
    }
}

fn tool_result(v: Value) -> Value {
    json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into())}],
        "structuredContent":v,"isError":false})
}
fn tool_error(e: ProtocolError) -> Value {
    let body = json!({"code":e.code,"message":e.message,"task_id":e.task_id});
    json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())}],
        "structuredContent":body,"isError":true})
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
        // Be conservative: many apparently observational operations record
        // custody or consume budgets. Only the terminal result and event-log
        // reads are guaranteed not to change task state.
        let title = t
            .wire_name()
            .strip_prefix("rex_")
            .unwrap_or(t.wire_name())
            .split('_')
            .map(|part| {
                let mut chars = part.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut descriptor = json!({"name":t.wire_name(),"title":format!("REX {title}"),
            "description":description,"inputSchema":schema});
        if matches!(t, ToolName::Events | ToolName::Result) {
            descriptor["annotations"] = json!({"readOnlyHint": true, "openWorldHint": false});
        }
        // Declare outputs only where the actual success shape is stable and
        // fully represented. Tool errors are separately marked isError.
        descriptor["outputSchema"] = match t {
            ToolName::Status => json!({
                "type":"object",
                "required":["task_id","state","task","operator_is_agent","host","lease","open_action","budgets","last_event_seq","operation","packet"],
                "properties":{
                    "task_id":{"type":"string"},
                    "state":{"enum":["created","active","verifying","completed","failed","cancelled"]},
                    "task":{"type":"string"},
                    "operator_is_agent":{"type":"boolean"},
                    "host":{"enum":["human","claude_code","codex","open_code","hermes","antigravity","generic_agent"]},
                    "lease":{"type":"object","required":["epoch","expires_ms_from_now","heartbeat_interval_ms"],"properties":{"epoch":{"type":"integer"},"expires_ms_from_now":{"type":"integer"},"heartbeat_interval_ms":{"type":"integer"}}},
                    "open_action":{"type":["object","null"]},
                    "budgets":{"type":"object"},
                    "last_event_seq":{"type":"integer"},
                    "operation":{"enum":["queued","prepared","committed","aborted","revoked","stale","conflict","external_host_required"]},
                    "packet":{"type":"object","required":["protocol_version","task_schema_version","kernel_schema_version","branch_id","lease_epoch","resume_nonce","idempotency_key"],"properties":{"protocol_version":{"type":"string"},"task_schema_version":{"type":"integer"},"kernel_schema_version":{"type":"integer"},"branch_id":{"type":"string"},"lease_epoch":{"type":"integer"},"resume_nonce":{"type":"integer"},"idempotency_key":{"type":"string"}}}
                }
            }),
            ToolName::Events => json!({
                "type":"object", "required":["events","last_seq"],
                "properties":{
                    "events":{"type":"array","items":{
                        "type":"object","required":["seq","ts_ms","kind","detail"],
                        "properties":{
                            "seq":{"type":"integer"},"ts_ms":{"type":"integer"},
                            "kind":{"type":"string"},"detail":{}
                        }
                    }},
                    "last_seq":{"type":"integer"}
                }
            }),
            ToolName::Result => json!({
                "type":"object", "required":["task_id","state"],
                "properties":{
                    "task_id":{"type":"string"},
                    "state":{"enum":["completed","failed","cancelled"]},
                    "output":{"type":"string"},
                    "proof_bundle":{"type":"object","additionalProperties":{"type":"string"}},
                    "terminal_reason":{"type":"string"}
                }
            }),
            _ => Value::Null,
        };
        if descriptor["outputSchema"].is_null() {
            descriptor.as_object_mut().unwrap().remove("outputSchema");
        }
        out.push(descriptor);
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
    fn inspection_output_schemas_match_success_shapes() {
        let descriptors = tool_descriptors();
        let find = |name: &str| descriptors.iter().find(|d| d["name"] == name).unwrap();
        let status_schema = &find("rex_status")["outputSchema"];
        assert_eq!(status_schema["type"], "object");
        assert_eq!(
            status_schema["properties"]["open_action"]["type"],
            json!(["object", "null"])
        );
        let events = &find("rex_events")["outputSchema"];
        assert_eq!(events["type"], "object");
        assert_eq!(
            events["properties"]["events"]["items"]["properties"]["ts_ms"]["type"],
            "integer"
        );
        assert_eq!(events["properties"]["last_seq"]["type"], "integer");
        let result = &find("rex_result")["outputSchema"];
        assert_eq!(
            result["properties"]["state"]["enum"],
            json!(["completed", "failed", "cancelled"])
        );
        assert_eq!(
            result["properties"]["proof_bundle"]["additionalProperties"]["type"],
            "string"
        );
        assert!(find("rex_execute").get("outputSchema").is_none());
        let (_d, mut server) = server();
        server
            .handle(rpc(
                1,
                "initialize",
                json!({"protocolVersion":MCP_PROTOCOL_VERSION}),
            ))
            .unwrap();
        let started = server.handle(rpc(2, "tools/call", json!({"name":"rex_execute","arguments":{
            "request_id":"schema-check","task":"inspect","host":"claude_code","operator_is_agent":true
        }}))).unwrap();
        let task_id = started["result"]["structuredContent"]["task_id"]
            .as_str()
            .unwrap();
        let actual_status = server
            .handle(rpc(
                6,
                "tools/call",
                json!({"name":"rex_status","arguments":{"task_id":task_id}}),
            ))
            .unwrap();
        let status_value = &actual_status["result"]["structuredContent"];
        for field in status_schema["required"].as_array().unwrap() {
            assert!(status_value.get(field.as_str().unwrap()).is_some());
        }
        assert_eq!(status_value["state"], "active");
        assert!(status_value["open_action"].is_null() || status_value["open_action"].is_object());
        for key in ["host", "operation"] {
            assert!(status_schema["properties"][key]["enum"]
                .as_array()
                .unwrap()
                .contains(&status_value[key]));
        }
        for key in ["lease", "packet"] {
            for field in status_schema["properties"][key]["required"]
                .as_array()
                .unwrap()
            {
                let field = field.as_str().unwrap();
                let actual = &status_value[key][field];
                assert!(!actual.is_null(), "missing {key}.{field}");
                let expected = status_schema["properties"][key]["properties"][field]["type"]
                    .as_str()
                    .unwrap();
                assert!(
                    match expected {
                        "string" => actual.is_string(),
                        "integer" => actual.is_u64(),
                        _ => false,
                    },
                    "wrong type for {key}.{field}"
                );
            }
        }
        let actual_events = server
            .handle(rpc(
                3,
                "tools/call",
                json!({"name":"rex_events","arguments":{"task_id":task_id}}),
            ))
            .unwrap();
        let value = &actual_events["result"]["structuredContent"];
        for field in events["required"].as_array().unwrap() {
            assert!(value.get(field.as_str().unwrap()).is_some());
        }
        for event in value["events"].as_array().unwrap() {
            for field in events["properties"]["events"]["items"]["required"]
                .as_array()
                .unwrap()
            {
                assert!(event.get(field.as_str().unwrap()).is_some());
            }
        }
        let stopped = server.handle(rpc(4,"tools/call",json!({"name":"rex_cancel","arguments":{
            "task_id":task_id,"capability":started["result"]["structuredContent"]["task_capability"]
        }}))).unwrap();
        assert_eq!(stopped["result"]["isError"], false);
        let actual_result = server
            .handle(rpc(
                5,
                "tools/call",
                json!({"name":"rex_result","arguments":{"task_id":task_id}}),
            ))
            .unwrap();
        let value = &actual_result["result"]["structuredContent"];
        for field in result["required"].as_array().unwrap() {
            assert!(value.get(field.as_str().unwrap()).is_some());
        }
        assert!(result["properties"]["state"]["enum"]
            .as_array()
            .unwrap()
            .contains(&value["state"]));
    }

    #[test]
    fn tool_titles_are_unique_and_host_readable() {
        let descriptors = tool_descriptors();
        let mut names = std::collections::HashSet::new();
        for descriptor in &descriptors {
            let name = descriptor["name"].as_str().unwrap();
            let title = descriptor["title"].as_str().unwrap();
            assert!(names.insert(name), "duplicate MCP tool name: {name}");
            assert!(title.starts_with("REX "), "missing REX title: {name}");
            assert!(title.len() > 4, "empty title: {name}");
        }
        assert_eq!(names.len(), ToolName::all().len());
    }

    #[test]
    fn tool_annotations_are_conservative_about_custody_side_effects() {
        let descriptors = tool_descriptors();
        let find = |name: &str| descriptors.iter().find(|d| d["name"] == name).unwrap();
        for name in ["rex_events", "rex_result"] {
            assert_eq!(find(name)["annotations"]["readOnlyHint"], true);
            assert_eq!(find(name)["annotations"]["openWorldHint"], false);
        }
        // Status reconciles custody; read and search may charge budgets.
        // Mutating tools must not be suggested as read-only to a host.
        for name in [
            "rex_status",
            "rex_read",
            "rex_search",
            "rex_edit",
            "rex_run",
            "rex_proof",
        ] {
            assert!(find(name).get("annotations").is_none());
        }
    }

    #[test]
    fn resource_quickstart_is_static_and_rejects_unknown_uris() {
        let (_d, mut s) = server();
        let init = s
            .handle(rpc(
                1,
                "initialize",
                json!({"protocolVersion": MCP_PROTOCOL_VERSION}),
            ))
            .unwrap();
        assert_eq!(
            init["result"]["capabilities"]["resources"]["listChanged"],
            false
        );
        let list = s.handle(rpc(2, "resources/list", json!({}))).unwrap();
        assert_eq!(
            list["result"]["resources"][0]["uri"],
            "rex://workflow/quickstart"
        );
        let read = s
            .handle(rpc(
                3,
                "resources/read",
                json!({"uri":"rex://workflow/quickstart"}),
            ))
            .unwrap();
        assert!(read["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("rex_execute"));
        let bad = s
            .handle(rpc(
                4,
                "resources/read",
                json!({"uri":"file:///etc/passwd"}),
            ))
            .unwrap();
        assert!(bad.get("error").is_some());
    }

    #[test]
    fn task_status_resource_template_reads_current_custody() {
        let (_d, mut s) = server();
        s.handle(rpc(
            1,
            "initialize",
            json!({"protocolVersion": MCP_PROTOCOL_VERSION}),
        ))
        .unwrap();
        let templates = s
            .handle(rpc(2, "resources/templates/list", json!({})))
            .unwrap();
        assert_eq!(
            templates["result"]["resourceTemplates"][0]["uriTemplate"],
            "rex://task/{task_id}/status"
        );
        let started = s
            .handle(rpc(
                3,
                "tools/call",
                json!({
                    "name": "rex_execute",
                    "arguments": {
                        "request_id": "status-resource-test",
                        "task": "inspect status",
                        "host": "claude_code",
                        "operator_is_agent": true
                    }
                }),
            ))
            .unwrap();
        let task_id = started["result"]["structuredContent"]["task_id"]
            .as_str()
            .unwrap();
        let uri = format!("rex://task/{task_id}/status");
        let read = s
            .handle(rpc(4, "resources/read", json!({"uri": uri})))
            .unwrap();
        let text = read["result"]["contents"][0]["text"].as_str().unwrap();
        let status: Value = serde_json::from_str(text).unwrap();
        assert_eq!(status["task_id"], task_id);
        assert_eq!(
            read["result"]["contents"][0]["mimeType"],
            "application/json"
        );
        let malformed = s
            .handle(rpc(
                5,
                "resources/read",
                json!({"uri": "rex://task/a/b/status"}),
            ))
            .unwrap();
        assert_eq!(malformed["error"]["code"], -32602);
    }

    #[test]
    fn task_events_resource_template_paginates_custody_log() {
        let (_d, mut s) = server();
        s.handle(rpc(
            1,
            "initialize",
            json!({"protocolVersion": MCP_PROTOCOL_VERSION}),
        ))
        .unwrap();
        let templates = s
            .handle(rpc(2, "resources/templates/list", json!({})))
            .unwrap();
        assert_eq!(
            templates["result"]["resourceTemplates"][1]["uriTemplate"],
            "rex://task/{task_id}/events/{after_seq}"
        );
        let started = s
            .handle(rpc(
                3,
                "tools/call",
                json!({
                    "name": "rex_execute",
                    "arguments": {
                        "request_id": "events-resource-test",
                        "task": "inspect events",
                        "host": "claude_code",
                        "operator_is_agent": true
                    }
                }),
            ))
            .unwrap();
        let task_id = started["result"]["structuredContent"]["task_id"]
            .as_str()
            .unwrap();
        let first = s
            .handle(rpc(
                4,
                "resources/read",
                json!({
                    "uri": format!("rex://task/{task_id}/events/0")
                }),
            ))
            .unwrap();
        let text = first["result"]["contents"][0]["text"].as_str().unwrap();
        let events: Value = serde_json::from_str(text).unwrap();
        assert!(events["events"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        let last_seq = events["last_seq"].as_u64().unwrap();
        let after = s
            .handle(rpc(
                5,
                "resources/read",
                json!({
                    "uri": format!("rex://task/{task_id}/events/{last_seq}")
                }),
            ))
            .unwrap();
        let later: Value =
            serde_json::from_str(after["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        assert!(later["events"].as_array().unwrap().is_empty());
        let bad = s
            .handle(rpc(
                6,
                "resources/read",
                json!({
                    "uri": format!("rex://task/{task_id}/events/not-a-sequence")
                }),
            ))
            .unwrap();
        assert_eq!(bad["error"]["code"], -32602);
    }

    #[test]
    fn task_result_resource_template_reads_terminal_proof() {
        let (_d, mut s) = server();
        s.handle(rpc(
            1,
            "initialize",
            json!({"protocolVersion": MCP_PROTOCOL_VERSION}),
        ))
        .unwrap();
        let templates = s
            .handle(rpc(2, "resources/templates/list", json!({})))
            .unwrap();
        assert_eq!(
            templates["result"]["resourceTemplates"][2]["uriTemplate"],
            "rex://task/{task_id}/result"
        );
        let started = s
            .handle(rpc(
                3,
                "tools/call",
                json!({
                    "name": "rex_execute",
                    "arguments": {
                        "request_id": "result-resource-test",
                        "task": "inspect result",
                        "host": "claude_code",
                        "operator_is_agent": true
                    }
                }),
            ))
            .unwrap();
        let task_id = started["result"]["structuredContent"]["task_id"]
            .as_str()
            .unwrap();
        let uri = format!("rex://task/{task_id}/result");
        let pending = s
            .handle(rpc(4, "resources/read", json!({"uri": uri})))
            .unwrap();
        assert_eq!(pending["error"]["data"]["code"], "no_result");
        let stopped = s
            .handle(rpc(
                5,
                "tools/call",
                json!({
                    "name": "rex_cancel",
                    "arguments": {
                        "task_id": task_id,
                        "capability": started["result"]["structuredContent"]["task_capability"]
                    }
                }),
            ))
            .unwrap();
        assert_eq!(stopped["result"]["isError"], false);
        let read = s
            .handle(rpc(6, "resources/read", json!({"uri": uri})))
            .unwrap();
        let text = read["result"]["contents"][0]["text"].as_str().unwrap();
        let result: Value = serde_json::from_str(text).unwrap();
        assert_eq!(result["task_id"], task_id);
        assert_eq!(result["state"], "cancelled");
        let malformed = s
            .handle(rpc(
                7,
                "resources/read",
                json!({
                    "uri": "rex://task/a/b/result"
                }),
            ))
            .unwrap();
        assert_eq!(malformed["error"]["code"], -32602);
    }

    #[test]
    fn prompt_discovery_and_get_after_initialize() {
        let (_d, mut s) = server();
        let init = s
            .handle(rpc(
                1,
                "initialize",
                json!({"protocolVersion": MCP_PROTOCOL_VERSION}),
            ))
            .unwrap();
        assert_eq!(
            init["result"]["capabilities"]["prompts"]["listChanged"],
            false
        );
        let list = s.handle(rpc(2, "prompts/list", json!({}))).unwrap();
        assert_eq!(list["result"]["prompts"][0]["name"], "rex_task_workflow");
        assert_eq!(list["result"]["prompts"][0]["arguments"][0]["name"], "task");
        let get = s
            .handle(rpc(
                3,
                "prompts/get",
                json!({"name":"rex_task_workflow", "arguments":{"task":"fix parser"}}),
            ))
            .unwrap();
        assert_eq!(get["result"]["messages"][0]["role"], "user");
        assert_eq!(get["result"]["messages"][0]["content"]["type"], "text");
        assert!(get["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("fix parser"));
        let no_task = s
            .handle(rpc(4, "prompts/get", json!({"name":"rex_task_workflow"})))
            .unwrap();
        assert_eq!(no_task["error"]["code"], -32602);
        let missing = s
            .handle(rpc(4, "prompts/get", json!({"name":"missing"})))
            .unwrap();
        assert!(missing.get("error").is_some());
    }

    #[test]
    fn progress_only_for_opted_in_active_long_calls() {
        let (_d, mut s) = server();
        let init = rpc(
            1,
            "initialize",
            json!({"protocolVersion":MCP_PROTOCOL_VERSION}),
        );
        let mut call = rpc(
            2,
            "tools/call",
            json!({"name":"rex_test","arguments":{},"_meta":{"progressToken":"gate-1"}}),
        );
        let input = format!("{}\n{}\n", init, call);
        let mut out = Vec::new();
        s.serve(std::io::Cursor::new(input), &mut out).unwrap();
        let rows: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1]["method"], "notifications/progress");
        assert_eq!(rows[1]["params"]["progressToken"], "gate-1");
        assert_eq!(rows[1]["params"]["progress"], 1);
        assert_eq!(rows[2]["id"], 2);
        // No token, malformed token, unknown/read-only tool, or missing id.
        call["params"].as_object_mut().unwrap().remove("_meta");
        assert!(progress_start(&call, true).is_none());
        call["params"]["_meta"] = json!({"progressToken":true});
        assert!(progress_start(&call, true).is_none());
        call["params"]["_meta"] = json!({"progressToken":7});
        assert_eq!(
            progress_start(&call, true).unwrap()["params"]["progressToken"],
            7
        );
        assert!(progress_start(&call, false).is_none());
        call["params"]["name"] = json!("rex_status");
        assert!(progress_start(&call, true).is_none());
        call.as_object_mut().unwrap().remove("id");
        assert!(progress_start(&call, true).is_none());
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
        assert_eq!(bad["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        // The client may disconnect if it cannot use the offered version.
        let (_d, mut s) = server();
        let missing = s.handle(rpc(1, "initialize", json!({}))).unwrap();
        assert_eq!(missing["error"]["code"], -32602);
        let ok = s
            .handle(rpc(
                2,
                "initialize",
                json!({"protocolVersion":MCP_COMPAT_PROTOCOL_VERSION}),
            ))
            .unwrap();
        assert_eq!(ok["result"]["protocolVersion"], MCP_COMPAT_PROTOCOL_VERSION);
        let listed = s.handle(rpc(3, "tools/list", json!({}))).unwrap();
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 19);
        let unknown = s
            .handle(rpc(
                4,
                "tools/call",
                json!({"name":"rex_hack","arguments":{}}),
            ))
            .unwrap();
        assert_eq!(unknown["error"]["data"]["code"], "unknown_tool");
    }
    #[test]
    fn malformed_stdio_frames_are_bounded_and_do_not_poison_later_requests() {
        let (_d, mut s) = server();
        let mut input = vec![b'a'; 1024 * 1024 + 10];
        input.extend_from_slice(b"\n");
        input.extend_from_slice(&[0xff, b'\n']);
        input.extend_from_slice(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.as_bytes());
        input.push(b'\n');
        let mut output = Vec::new();
        s.serve(std::io::Cursor::new(input), &mut output).unwrap();
        let results: Vec<Value> = output
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["error"]["code"], -32700);
        assert_eq!(results[1]["error"]["code"], -32700);
        assert_eq!(results[2]["result"], json!({}));
    }
    #[test]
    fn malformed_requests_receive_errors_instead_of_becoming_notifications() {
        let (_d, mut s) = server();
        for request in [
            json!(null),
            json!([]),
            json!({"jsonrpc":"2.0"}),
            json!({"jsonrpc":"2.0", "id": 1, "method": 3}),
            json!({"jsonrpc":"2.0", "method": ""}),
        ] {
            let response = s.handle(request).expect("malformed request response");
            assert_eq!(response["error"]["code"], -32600);
            assert!(response["id"].is_null());
        }
        let blocked = s.handle(rpc(2, "tools/list", json!({}))).unwrap();
        assert_eq!(blocked["error"]["data"]["code"], "unauthorized");
    }
    #[test]
    fn execution_failures_are_visible_as_mcp_tool_errors() {
        let (_d, mut s) = server();
        let before = s.handle(rpc(1, "tools/list", json!({}))).unwrap();
        assert_eq!(before["error"]["data"]["code"], "unauthorized");
        s.handle(rpc(
            2,
            "initialize",
            json!({"protocolVersion":MCP_PROTOCOL_VERSION}),
        ))
        .unwrap();
        let unknown = s
            .handle(rpc(3, "tools/call", json!({"name":"missing"})))
            .unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
        let malformed = s
            .handle(rpc(4, "tools/call", json!({"name":"rex_status"})))
            .unwrap();
        assert_eq!(malformed["error"]["code"], -32602);
        let not_found = s
            .handle(rpc(
                5,
                "tools/call",
                json!({"name":"rex_status", "arguments":{"task_id":"absent"}}),
            ))
            .unwrap();
        assert_eq!(not_found["result"]["isError"], true);
        assert_eq!(
            not_found["result"]["structuredContent"]["code"],
            "task_not_found"
        );
        assert!(not_found["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("task_not_found"));
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
        let first = s
            .handle(json!({"jsonrpc":"2.0", "id":0,
            "method":"initialize", "params":{"protocolVersion":MCP_PROTOCOL_VERSION}}))
            .unwrap();
        assert_eq!(first["id"], 0);
        assert!(first.get("result").is_some());
        let repeat = s
            .handle(json!({"jsonrpc":"2.0", "id":"a",
            "method":"initialize", "params":{"protocolVersion":MCP_PROTOCOL_VERSION}}))
            .unwrap();
        assert_eq!(repeat["id"], "a");
        assert_eq!(repeat["error"]["code"], -32602);
        let still_ready = s.handle(rpc(2, "tools/list", json!({}))).unwrap();
        assert!(still_ready["result"]["tools"].is_array());
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
