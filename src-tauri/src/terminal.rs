//! Interactive terminal backend: PTY sessions bound to the workspace.
//!
//! Each terminal spawns the user's shell in a pseudo-terminal via
//! `portable-pty`. The sandbox is the rex-tools discipline applied to
//! interactive use:
//!
//! - the PTY starts in a workspace directory validated to sit under the
//!   agent runs root (no arbitrary cwd);
//! - the environment is cleared except for a minimal PATH (and TERM), so no
//!   host secrets leak into the shell;
//! - the child runs as the invoking user with no privilege escalation.
//!
//! This is the user's own terminal, not a tool sandbox: network access is
//! allowed because `git pull` and `npm install` are the point. The boundary
//! is workspace confinement + clean env, enforced in Rust.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

/// A live PTY session.
struct TerminalSession {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
}

/// Manages PTY sessions by ID. Held in Tauri state.
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, TerminalSession>>,
    next_id: Mutex<u64>,
}

impl TerminalManager {
    pub fn new() -> Self {
        TerminalManager {
            sessions: Mutex::new(HashMap::new()),
            next_id: Mutex::new(0),
        }
    }

    /// Validate that `workspace` sits under the agent runs root.
    fn validate_workspace(workspace: &Path, runs_root: &Path) -> Result<PathBuf, String> {
        let canonical = workspace
            .canonicalize()
            .map_err(|e| format!("workspace not found: {e}"))?;
        let root = runs_root
            .canonicalize()
            .map_err(|e| format!("runs root not found: {e}"))?;
        if !canonical.starts_with(&root) {
            return Err("workspace must sit under the agent runs root".to_string());
        }
        Ok(canonical)
    }

    /// Spawn a shell in a PTY bound to `workspace`. Returns the terminal ID.
    pub fn spawn(
        &self,
        workspace: PathBuf,
        runs_root: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<String, String> {
        let cwd = Self::validate_workspace(&workspace, runs_root)?;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("cannot open pty: {e}"))?;

        // Minimal clean env: PATH + TERM only. No host secrets.
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.cwd(&cwd);
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        } else {
            cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        }
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("cannot spawn shell: {e}"))?;
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("cannot take pty writer: {e}"))?;

        let mut id_guard = self.next_id.lock().map_err(|e| e.to_string())?;
        *id_guard += 1;
        let id = format!("term-{}", *id_guard);
        drop(id_guard);

        self.sessions.lock().map_err(|e| e.to_string())?.insert(
            id.clone(),
            TerminalSession {
                master: pair.master,
                child,
                writer,
            },
        );
        Ok(id)
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
        let mut sessions = self.sessions.lock().map_err(|e| e.to_string())?;
        let session = sessions.get_mut(id).ok_or("unknown terminal")?;
        use std::io::Write;
        session
            .writer
            .write_all(data)
            .map_err(|e| format!("pty write failed: {e}"))?;
        session
            .writer
            .flush()
            .map_err(|e| format!("pty flush failed: {e}"))?;
        Ok(())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let sessions = self.sessions.lock().map_err(|e| e.to_string())?;
        let session = sessions.get(id).ok_or("unknown terminal")?;
        session
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("pty resize failed: {e}"))?;
        Ok(())
    }

    pub fn kill(&self, id: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().map_err(|e| e.to_string())?;
        if let Some(mut session) = sessions.remove(id) {
            let _ = session.child.kill();
            let _ = session.child.wait();
        }
        Ok(())
    }

    /// Clone the PTY reader for background draining. The Tauri layer spawns
    /// a thread that forwards bytes as `terminal-output-{id}` events.
    pub fn clone_reader(&self, id: &str) -> Result<Box<dyn Read + Send>, String> {
        let sessions = self.sessions.lock().map_err(|e| e.to_string())?;
        let session = sessions.get(id).ok_or("unknown terminal")?;
        session
            .master
            .try_clone_reader()
            .map_err(|e| format!("cannot clone pty reader: {e}"))
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared handle for Tauri state.
pub type SharedTerminals = Arc<TerminalManager>;
