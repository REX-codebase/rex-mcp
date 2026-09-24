use rex_tools::{CallState, ErrorKind, ToolRequest, ToolRuntime};
use std::fs;
fn workspace() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("rex-tools-it-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}
#[test]
fn read_search_write_and_receipts_share_one_protocol() {
    let root = workspace();
    fs::write(root.join("note.txt"), "alpha\nbeta\n").unwrap();
    let rt = ToolRuntime::new(&root).unwrap();
    let read = rt
        .prepare(ToolRequest::ReadFile {
            path: "note.txt".into(),
            offset: None,
            limit: None,
        })
        .unwrap();
    assert!(!read.approval_required);
    let result = rt.execute(&read.call_id);
    assert!(result.ok);
    assert_eq!(result.state, CallState::Executed);
    assert_eq!(result.receipt.target.as_deref(), Some("note.txt"));
    let search = rt
        .prepare(ToolRequest::SearchFiles {
            query: "beta".into(),
            path: None,
            max_results: Some(5),
            regex: None,
            include: None,
        })
        .unwrap();
    assert!(rt
        .execute(&search.call_id)
        .output
        .unwrap()
        .contains("note.txt:2:beta"));
    let edit = rt
        .prepare(ToolRequest::EditFile {
            path: "note.txt".into(),
            expected: "beta".into(),
            replacement: "gamma".into(),
            replace_all: false,
        })
        .unwrap();
    rt.resolve_approval(&edit.call_id, true).unwrap();
    let result = rt.execute(&edit.call_id);
    assert!(result.ok);
    assert!(result.receipt.diff.unwrap().contains("+gamma"));
    assert_eq!(
        rt.execute(&edit.call_id).error.unwrap().kind,
        ErrorKind::AlreadyExecuted
    );
}
