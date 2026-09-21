//! Custody-enforcing wrapper over the rex-tools runtime.
//!
//! Every call carries the worker's `CapabilityToken`. The wrapper verifies
//! the token (live grant, current epoch, matching scope), checks the
//! request against the granted capability set, and only then touches the
//! underlying runtime. A scope escape seizes custody as
//! `Violation::CapabilityEscape`. Trusted human approval for risky calls is
//! unchanged: custody can narrow what is asked, never approve what was
//! asked.

use crate::capability::CapabilityToken;
use crate::registry::{CustodyError, CustodyRegistry};
use crate::state::Violation;
use rex_tools::{CallState, PreparedCall, ToolRequest, ToolResult, ToolRuntime};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub enum CustodyToolError {
    Custody(CustodyError),
    Tool(rex_tools::ToolError),
}

impl std::fmt::Display for CustodyToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Custody(e) => write!(f, "custody: {e}"),
            Self::Tool(e) => write!(f, "tool: {:?} {}", e.kind, e.detail),
        }
    }
}
impl std::error::Error for CustodyToolError {}

#[derive(Clone)]
pub struct CustodiedToolRuntime {
    inner: ToolRuntime,
    registry: Arc<Mutex<CustodyRegistry>>,
}

impl CustodiedToolRuntime {
    pub fn new(inner: ToolRuntime, registry: Arc<Mutex<CustodyRegistry>>) -> Self {
        Self { inner, registry }
    }

    /// Prepare a call under custody. Scope decisions happen here, before
    /// the request can even become a pending call.
    pub fn prepare(
        &self,
        token: &CapabilityToken,
        request: ToolRequest,
        now_ms: u128,
    ) -> Result<PreparedCall, CustodyToolError> {
        let mut reg = self
            .registry
            .lock()
            .map_err(|_| CustodyToolError::Custody(CustodyError::Io("registry poisoned".into())))?;
        let grant = reg
            .verify_token(token, now_ms)
            .map_err(CustodyToolError::Custody)?;
        if let Err(denial) = grant.capabilities.permits(&request) {
            let v = Violation::CapabilityEscape {
                detail: denial.to_string(),
            };
            let _ = reg.violate(&token.grant_id, v, now_ms);
            return Err(CustodyToolError::Custody(CustodyError::Quarantined(
                Violation::CapabilityEscape {
                    detail: denial.to_string(),
                },
            )));
        }
        drop(reg);
        self.inner.prepare(request).map_err(CustodyToolError::Tool)
    }

    /// Trusted UI decision passthrough. Custody adds no approval authority;
    /// the human boundary in rex-tools is untouched.
    pub fn resolve_approval(
        &self,
        call_id: &str,
        approved: bool,
    ) -> Result<CallState, CustodyToolError> {
        self.inner
            .resolve_approval(call_id, approved)
            .map_err(CustodyToolError::Tool)
    }

    pub fn cancel(&self, call_id: &str) -> Result<CallState, CustodyToolError> {
        self.inner.cancel(call_id).map_err(CustodyToolError::Tool)
    }

    /// Execute a prepared call. Token is re-verified at execution: a grant
    /// released or quarantined between prepare and execute stops the call.
    pub fn execute(&self, token: &CapabilityToken, call_id: &str, now_ms: u128) -> ToolResult {
        let checked = {
            let mut reg = match self.registry.lock() {
                Ok(r) => r,
                Err(_) => {
                    return custody_blocked_result(call_id, "registry poisoned");
                }
            };
            reg.verify_token(token, now_ms)
        };
        match checked {
            Ok(_) => self.inner.execute(call_id),
            Err(e) => custody_blocked_result(call_id, &format!("custody refused execution: {e}")),
        }
    }
}

fn custody_blocked_result(call_id: &str, detail: &str) -> ToolResult {
    ToolResult {
        call_id: call_id.to_string(),
        ok: false,
        tool: "custody".into(),
        state: CallState::Denied,
        output: None,
        error: Some(rex_tools::ToolError {
            kind: rex_tools::ErrorKind::PolicyDenied,
            detail: detail.to_string(),
        }),
        receipt: rex_tools::AuditReceipt {
            started_at_ms: 0,
            duration_ms: 0,
            workspace_root: String::new(),
            target: None,
            command: None,
            exit_code: None,
            bytes_read: 0,
            bytes_written: 0,
            output_truncated: false,
            diff: None,
            redactions: 0,
        },
    }
}
