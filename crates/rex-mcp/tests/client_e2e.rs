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

    // Human stop is final even for an agent-operator task.
    let cancel = client
        .call_tool(
            "rex_cancel",
            json!({ "task_id": task_id, "reason": "human stop from shell" }),
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
