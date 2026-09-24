//! Multi-file patches in the envelope format Codex-family models are
//! trained to emit:
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path/new.rs
//! +line
//! *** Update File: path/old.rs
//! *** Move to: path/renamed.rs      (optional)
//! @@ optional context header
//!  context line
//! -removed line
//! +added line
//! *** Delete File: path/gone.rs
//! *** End Patch
//! ```
//!
//! Parsing and planning are pure: the caller supplies file contents and gets
//! back the complete set of resulting files, so the whole patch is checked
//! before a single byte is written. Hunks are located with the same
//! tolerant, unique-match planner as `edit_file`. Implementation is REX's
//! own; only the text format is shared with other harnesses.

use crate::fuzzy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<Hunk>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Context + removed lines, in order.
    pub old: Vec<String>,
    /// Context + added lines, in order.
    pub new: Vec<String>,
}

/// Final effect of a planned patch on one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Write {
        path: String,
        original: Option<String>,
        content: String,
    },
    Delete {
        path: String,
        original: String,
    },
}

pub fn parse(text: &str) -> Result<Vec<Op>, String> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == "*** Begin Patch")
        .ok_or("patch must start with '*** Begin Patch'")?;
    let end = lines
        .iter()
        .rposition(|l| l.trim() == "*** End Patch")
        .ok_or("patch must end with '*** End Patch'")?;
    if end <= start {
        return Err("'*** End Patch' comes before '*** Begin Patch'".into());
    }
    let body = &lines[start + 1..end];
    let mut ops = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let line = body[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = clean_path(path)?;
            i += 1;
            let mut content = Vec::new();
            while i < body.len() && !body[i].starts_with("*** ") {
                let l = body[i];
                let Some(rest) = l.strip_prefix('+') else {
                    return Err(format!(
                        "line {} in Add File {path}: every line must start with '+'",
                        start + i + 2
                    ));
                };
                content.push(rest);
                i += 1;
            }
            let mut text = content.join("\n");
            if !content.is_empty() {
                text.push('\n');
            }
            ops.push(Op::Add {
                path,
                content: text,
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(Op::Delete {
                path: clean_path(path)?,
            });
            i += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = clean_path(path)?;
            i += 1;
            let mut move_to = None;
            if i < body.len() {
                if let Some(dest) = body[i].strip_prefix("*** Move to: ") {
                    move_to = Some(clean_path(dest)?);
                    i += 1;
                }
            }
            let mut hunks = Vec::new();
            let mut cur = Hunk {
                old: Vec::new(),
                new: Vec::new(),
            };
            let mut changed = false;
            while i < body.len()
                && !(body[i].starts_with("*** ") && body[i].trim() != "*** End of File")
            {
                let l = body[i];
                i += 1;
                if l.trim() == "*** End of File" {
                    continue;
                }
                if l.starts_with("@@") {
                    if !cur.old.is_empty() || !cur.new.is_empty() {
                        hunks.push(std::mem::replace(
                            &mut cur,
                            Hunk {
                                old: Vec::new(),
                                new: Vec::new(),
                            },
                        ));
                    }
                    continue;
                }
                match l.chars().next() {
                    Some('-') => {
                        cur.old.push(l[1..].to_string());
                        changed = true;
                    }
                    Some('+') => {
                        cur.new.push(l[1..].to_string());
                        changed = true;
                    }
                    Some(' ') => {
                        cur.old.push(l[1..].to_string());
                        cur.new.push(l[1..].to_string());
                    }
                    // Models often drop the leading space on blank context.
                    None => {
                        cur.old.push(String::new());
                        cur.new.push(String::new());
                    }
                    Some(_) => {
                        return Err(format!(
                        "line {} in Update File {path}: hunk lines must start with ' ', '-' or '+'",
                        start + i + 1
                    ))
                    }
                }
            }
            if !cur.old.is_empty() || !cur.new.is_empty() {
                hunks.push(cur);
            }
            if !changed && move_to.is_none() {
                return Err(format!("Update File {path} has no changes"));
            }
            ops.push(Op::Update {
                path,
                move_to,
                hunks,
            });
        } else {
            return Err(format!(
                "line {}: expected '*** Add File:', '*** Update File:' or '*** Delete File:', got {:?}",
                start + i + 2,
                truncate(line)
            ));
        }
    }
    if ops.is_empty() {
        return Err("patch contains no file operations".into());
    }
    Ok(ops)
}

fn truncate(s: &str) -> String {
    s.chars().take(80).collect()
}

fn clean_path(raw: &str) -> Result<String, String> {
    let p = raw.trim();
    if p.is_empty() {
        return Err("empty path in patch header".into());
    }
    Ok(p.to_string())
}

/// Plan every operation against the current files. `read` returns the
/// current content of a path, or `None` if it does not exist. Paths are
/// processed in order and later operations see earlier results, so a patch
/// may update a file it just added. Any failure rejects the whole patch.
pub fn plan(
    ops: &[Op],
    mut read: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<Vec<Change>, String> {
    use std::collections::BTreeMap;
    // path -> (original on disk, current planned state: None = deleted)
    let mut state: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut load = |path: &str,
                    state: &mut BTreeMap<String, (Option<String>, Option<String>)>,
                    order: &mut Vec<String>|
     -> Result<Option<String>, String> {
        if let Some((_, cur)) = state.get(path) {
            return Ok(cur.clone());
        }
        let orig = read(path)?;
        state.insert(path.to_string(), (orig.clone(), orig.clone()));
        order.push(path.to_string());
        Ok(orig)
    };
    for op in ops {
        match op {
            Op::Add { path, content } => {
                if load(path, &mut state, &mut order)?.is_some() {
                    return Err(format!(
                        "Add File {path}: file already exists; use Update File"
                    ));
                }
                state.get_mut(path).expect("loaded").1 = Some(content.clone());
            }
            Op::Delete { path } => {
                if load(path, &mut state, &mut order)?.is_none() {
                    return Err(format!("Delete File {path}: file does not exist"));
                }
                state.get_mut(path).expect("loaded").1 = None;
            }
            Op::Update {
                path,
                move_to,
                hunks,
            } => {
                let Some(mut text) = load(path, &mut state, &mut order)? else {
                    return Err(format!("Update File {path}: file does not exist"));
                };
                for (n, h) in hunks.iter().enumerate() {
                    text = apply_hunk(&text, h)
                        .map_err(|e| format!("Update File {path}, hunk {}: {e}", n + 1))?;
                }
                match move_to {
                    Some(dest) if dest != path => {
                        if load(dest, &mut state, &mut order)?.is_some() {
                            return Err(format!("Move to {dest}: destination already exists"));
                        }
                        state.get_mut(path).expect("loaded").1 = None;
                        state.get_mut(dest.as_str()).expect("loaded").1 = Some(text);
                    }
                    _ => state.get_mut(path).expect("loaded").1 = Some(text),
                }
            }
        }
    }
    let mut changes = Vec::new();
    for path in order {
        let (orig, cur) = state.remove(&path).expect("tracked");
        match (orig, cur) {
            (None, None) => {}
            (Some(o), None) => changes.push(Change::Delete { path, original: o }),
            (o, Some(c)) => {
                if o.as_deref() != Some(c.as_str()) {
                    changes.push(Change::Write {
                        path,
                        original: o,
                        content: c,
                    });
                }
            }
        }
    }
    Ok(changes)
}

fn apply_hunk(text: &str, h: &Hunk) -> Result<String, String> {
    if h.old.iter().all(|l| l.trim().is_empty()) {
        // Pure insertion with no anchor: append at end of file.
        if h.old.is_empty() {
            let mut out = text.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for l in &h.new {
                out.push_str(l);
                out.push('\n');
            }
            return Ok(out);
        }
        return Err("hunk has no non-blank context or removed lines to anchor on".into());
    }
    let old = format!("{}\n", h.old.join("\n"));
    let new = if h.new.is_empty() {
        String::new()
    } else {
        format!("{}\n", h.new.join("\n"))
    };
    // A file without a trailing newline is planned as if it had one, and
    // the missing final newline is restored afterwards.
    let had_nl = text.ends_with('\n');
    let padded;
    let base = if had_nl {
        text
    } else {
        padded = format!("{text}\n");
        &padded
    };
    let attempt = fuzzy::plan_edit(base, &old, &new, false).map(|mut p| {
        if !had_nl && p.new_content.ends_with('\n') {
            p.new_content.pop();
        }
        p
    });
    attempt.map(|p| p.new_content).map_err(|e| match e {
        fuzzy::PlanError::NotFound { hint } => format!("context not found; {hint}"),
        fuzzy::PlanError::Ambiguous { count, lines, .. } => {
            format!("context matches {count} places (lines {lines:?}); add more context lines")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn run(files: &[(&str, &str)], patch: &str) -> Result<Vec<Change>, String> {
        let map: HashMap<String, String> = files
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect();
        let ops = parse(patch)?;
        plan(&ops, |p| Ok(map.get(p).cloned()))
    }

    #[test]
    fn add_update_delete_move_in_one_patch() {
        let patch = "*** Begin Patch\n*** Add File: new.txt\n+hello\n+world\n*** Update File: a.rs\n@@ fn main\n fn main() {\n-    old();\n+    new();\n }\n*** Delete File: gone.txt\n*** Update File: b.txt\n*** Move to: c.txt\n@@\n-x\n+y\n*** End Patch";
        let ch = run(
            &[
                ("a.rs", "fn main() {\n    old();\n}\n"),
                ("gone.txt", "bye\n"),
                ("b.txt", "x\n"),
            ],
            patch,
        )
        .unwrap();
        assert!(ch.contains(&Change::Write {
            path: "new.txt".into(),
            original: None,
            content: "hello\nworld\n".into()
        }));
        assert!(ch.contains(&Change::Write {
            path: "a.rs".into(),
            original: Some("fn main() {\n    old();\n}\n".into()),
            content: "fn main() {\n    new();\n}\n".into()
        }));
        assert!(ch.contains(&Change::Delete {
            path: "gone.txt".into(),
            original: "bye\n".into()
        }));
        assert!(ch.contains(&Change::Delete {
            path: "b.txt".into(),
            original: "x\n".into()
        }));
        assert!(ch.contains(&Change::Write {
            path: "c.txt".into(),
            original: None,
            content: "y\n".into()
        }));
    }

    #[test]
    fn one_bad_hunk_rejects_the_whole_patch() {
        let patch = "*** Begin Patch\n*** Add File: ok.txt\n+fine\n*** Update File: a.txt\n@@\n-not there\n+x\n*** End Patch";
        let e = run(&[("a.txt", "something else\n")], patch).unwrap_err();
        assert!(
            e.contains("hunk 1") && e.contains("context not found"),
            "{e}"
        );
    }

    #[test]
    fn ambiguous_context_is_refused() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-dup\n+x\n*** End Patch";
        let e = run(&[("a.txt", "dup\nmid\ndup\n")], patch).unwrap_err();
        assert!(e.contains("matches 2 places"), "{e}");
    }

    #[test]
    fn multiple_hunks_apply_in_order_and_tolerate_indent_drift() {
        let patch = "*** Begin Patch\n*** Update File: m.py\n@@ def a\n def a():\n-    return 1\n+    return 10\n@@ def b\n def b():\n-  return 2\n+  return 20\n*** End Patch";
        let ch = run(
            &[("m.py", "def a():\n    return 1\n\ndef b():\n    return 2\n")],
            patch,
        )
        .unwrap();
        match &ch[0] {
            Change::Write { content, .. } => {
                assert_eq!(
                    content,
                    "def a():\n    return 10\n\ndef b():\n    return 20\n"
                )
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn blank_context_without_space_and_missing_final_newline() {
        let patch = "*** Begin Patch\n*** Update File: t.txt\n@@\n a\n\n-b\n+B\n*** End Patch";
        let ch = run(&[("t.txt", "a\n\nb")], patch).unwrap();
        match &ch[0] {
            Change::Write { content, .. } => assert_eq!(content, "a\n\nB"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn deletion_in_file_without_final_newline_leaves_no_blank_line() {
        let patch = "*** Begin Patch\n*** Update File: t.txt\n@@\n a\n-b\n c\n*** End Patch";
        let ch = run(&[("t.txt", "a\nb\nc")], patch).unwrap();
        match &ch[0] {
            Change::Write { content, .. } => assert_eq!(content, "a\nc"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn structural_errors_are_specific() {
        assert!(parse("no envelope").unwrap_err().contains("Begin Patch"));
        assert!(parse("*** Begin Patch\n*** End Patch")
            .unwrap_err()
            .contains("no file operations"));
        assert!(
            parse("*** Begin Patch\n*** Add File: x\nnoplus\n*** End Patch")
                .unwrap_err()
                .contains("must start with '+'")
        );
        assert!(parse("*** Begin Patch\nrandom\n*** End Patch")
            .unwrap_err()
            .contains("expected"));
        let e = run(
            &[("x", "1\n")],
            "*** Begin Patch\n*** Add File: x\n+2\n*** End Patch",
        )
        .unwrap_err();
        assert!(e.contains("already exists"));
        let e = run(&[], "*** Begin Patch\n*** Delete File: y\n*** End Patch").unwrap_err();
        assert!(e.contains("does not exist"));
    }

    #[test]
    fn add_then_update_same_file_sees_planned_state() {
        let patch = "*** Begin Patch\n*** Add File: n.txt\n+one\n*** Update File: n.txt\n@@\n-one\n+two\n*** End Patch";
        let ch = run(&[], patch).unwrap();
        assert_eq!(
            ch,
            vec![Change::Write {
                path: "n.txt".into(),
                original: None,
                content: "two\n".into()
            }]
        );
    }
}
