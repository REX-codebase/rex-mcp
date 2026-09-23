//! OS-level confinement for spawned commands.
//!
//! The audit (finding 5) showed the argv[0] denylist is a speed bump, not
//! a boundary: `/usr/bin/env bash -c ...` walks straight past it. The real
//! wall is built here: every spawned child enters a fresh user namespace
//! (its root maps back to the real uid, so workspace file access keeps
//! working but it holds no host credentials), a fresh network namespace
//! with no routes at all (default-deny egress), and fresh IPC/UTS
//! namespaces.
//!
//! When the kernel refuses unprivileged namespaces the caller decides:
//! run without confinement and say so in the receipt, or refuse. This
//! module never hides the outcome.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxStatus {
    /// User + network + IPC + UTS namespaces applied: no host credentials
    /// and no network route inside the child.
    Applied,
    /// The kernel refused namespace setup; the command ran with rlimits
    /// and process-group isolation only. The string is the exact reason.
    Unavailable(String),
}

impl SandboxStatus {
    pub fn label(&self) -> String {
        match self {
            SandboxStatus::Applied => "applied: userns+netns+ipc+uts".into(),
            SandboxStatus::Unavailable(reason) => format!("unavailable: {reason}"),
        }
    }
}

/// Error marker so the spawner can tell "sandbox could not be set up"
/// apart from an ordinary spawn failure and decide to degrade honestly.
pub const SANDBOX_ERROR_PREFIX: &str = "rex-sandbox:";

/// Apply the namespace sandbox inside the child, between fork and exec.
/// Async-signal-context: keep this to direct syscalls and small writes.
#[cfg(target_os = "linux")]
pub fn apply_in_child() -> Result<(), String> {
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    if unsafe { libc::unshare(libc::CLONE_NEWUSER) } != 0 {
        return Err(format!(
            "unshare(CLONE_NEWUSER): {}",
            std::io::Error::last_os_error()
        ));
    }
    // Map namespace-root back to the real uid/gid so workspace ownership
    // and permissions behave exactly as outside. setgroups must be denied
    // before the gid map can be written.
    std::fs::write("/proc/self/setgroups", "deny").map_err(|e| format!("setgroups deny: {e}"))?;
    std::fs::write("/proc/self/uid_map", format!("0 {uid} 1"))
        .map_err(|e| format!("uid_map: {e}"))?;
    std::fs::write("/proc/self/gid_map", format!("0 {gid} 1"))
        .map_err(|e| format!("gid_map: {e}"))?;
    if unsafe { libc::unshare(libc::CLONE_NEWNET | libc::CLONE_NEWIPC | libc::CLONE_NEWUTS) } != 0 {
        return Err(format!(
            "unshare(CLONE_NEWNET|CLONE_NEWIPC|CLONE_NEWUTS): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn apply_in_child() -> Result<(), String> {
    Err("namespace sandbox is only implemented on linux".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_are_explicit() {
        assert!(SandboxStatus::Applied.label().starts_with("applied"));
        assert!(SandboxStatus::Unavailable("x".into())
            .label()
            .contains("unavailable: x"));
    }
}
