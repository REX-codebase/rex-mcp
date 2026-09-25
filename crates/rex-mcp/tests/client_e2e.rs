//! End-to-end: the shell client drives the real rex-mcp binary over stdio,
//! through the same handshake and tool path a host would use.

use rex_mcp::client::{call_tool_once, McpStdioClient};
use serde_json::json;

fn bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_rex-mcp"))
}

#[test]
fn shell_client_runs_a_full_task_lifecycle_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut client = McpStdioClient::spawn(&bin(), &state, &workspace).unwrap();

    let exec = client
        .call_tool(
            "rex_execute",
            json!({
                "request_id": "ui-req-1",
                "task": "inspect the workspace",
                "host": "generic_agent",
                "operator_is_agent": true
            }),
        )
        .unwrap();
    let task_id = exec
        .get("task_id")
        .and_then(|v| v.as_str())
        .expect("execute returns a task id")
        .to_string();
    assert_eq!(
        exec.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        "active",
        "a fresh task starts active"
    );

    let status = client
        .call_tool("rex_status", json!({ "task_id": task_id }))
        .unwrap();
    assert_eq!(
        status.get("task_id").and_then(|v| v.as_str()),
        Some(task_id.as_str())
    );
    assert_eq!(
        status.get("operator_is_agent").and_then(|v| v.as_bool()),
        Some(true),
        "custody records the declared agent operator"
    );

    let events = client
        .call_tool("rex_events", json!({ "task_id": task_id }))
        .unwrap();
    assert!(events
        .get("events")
        .and_then(|v| v.as_array())
        .map(|e| !e.is_empty())
        .unwrap_or(false));

    // Operator cancel without the per-task capability is unauthorized:
    // task id plus the (status-visible) lease epoch authorize nothing.
    let denied = client.call_tool(
        "rex_cancel",
        json!({ "task_id": task_id, "reason": "forged operator cancel" }),
    );
    assert!(denied.is_err(), "capability-less cancel must be denied");

    // Human stop is a distinct authority: the trusted local launcher reads
    // the daemon's human-stop token from the state dir. Final even for an
    // agent-operator task.
    let token = std::fs::read_to_string(state.join("human-stop-token"))
        .expect("daemon issues a human-stop token");
    let cancel = client
        .call_tool(
            "rex_human_stop",
            json!({ "task_id": task_id, "human_token": token.trim(), "reason": "human stop from shell" }),
        )
        .unwrap();
    assert_eq!(
        cancel.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        "cancelled"
    );
    let after = client
        .call_tool("rex_status", json!({ "task_id": task_id }))
        .unwrap();
    assert_eq!(
        after.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        "cancelled"
    );
}

#[test]
fn one_shot_call_matches_persistent_session() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let exec = call_tool_once(
        &bin(),
        &state,
        &workspace,
        "rex_execute",
        json!({
            "request_id": "ui-req-2",
            "task": "one shot task",
            "host": "human",
            "operator_is_agent": false
        }),
    )
    .unwrap();
    assert_eq!(
        exec.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        "active"
    );
}

#[cfg(unix)]
#[test]
fn shell_client_rejects_an_incompatible_mcp_protocol_version() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("fake-mcp");
    // A server that replies successfully but negotiates a version this
    // bundled client does not speak must not receive initialized or calls.
    std::fs::write(
        &fake,
        "#!/bin/sh\nread request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2099-01-01\"}}'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fake).unwrap().permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(&fake, perms).unwrap();
    let error = match McpStdioClient::spawn(&fake, tmp.path(), tmp.path()) {
        Ok(_) => panic!("incompatible protocol must be rejected"),
        Err(error) => error,
    };
    assert!(error.contains("unsupported protocol version"), "{error}");
    assert!(error.contains("2099-01-01"), "{error}");
}

#[cfg(unix)]
#[test]
fn shell_client_times_out_when_server_never_replies() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("silent-mcp");
    std::fs::write(&fake, "#!/bin/sh\nread request\nexec sleep 35\n").unwrap();
    let mut perms = std::fs::metadata(&fake).unwrap().permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(&fake, perms).unwrap();
    let start = std::time::Instant::now();
    let error = match McpStdioClient::spawn(&fake, tmp.path(), tmp.path()) {
        Ok(_) => panic!("silent server must time out"),
        Err(error) => error,
    };
    assert!(error.contains("reply timed out for initialize"), "{error}");
    assert!(start.elapsed() < std::time::Duration::from_secs(34));
}
