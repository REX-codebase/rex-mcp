//! Bounded orchestration between provider turns and the capability runtime.
//! Networking is supplied by the caller. The loop itself owns limits,
//! cancellation, approval suspension, result redaction, and receipts.
use crate::conversation::{decode_turn, NormalizedTurn, ToolOutcome};
use crate::providers::ProviderProtocol;
use rex_tools::{AuditReceipt, PreparedCall, ToolResult, ToolRuntime};
use serde::Serialize;

pub const DEFAULT_MAX_STEPS: usize = 12;
pub const HARD_MAX_STEPS: usize = 32;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoopEvent {
    ModelText { text: String },
    ApprovalRequired { call: PreparedCall },
    ToolFinished { result: ToolResult },
    Finished,
    Cancelled,
    LimitReached { max_steps: usize },
}

pub struct AgentLoop {
    protocol: ProviderProtocol,
    max_steps: usize,
    steps: usize,
    cancelled: bool,
}
impl AgentLoop {
    pub fn new(protocol: ProviderProtocol, max_steps: usize) -> Self {
        Self {
            protocol,
            max_steps: max_steps.clamp(1, HARD_MAX_STEPS),
            steps: 0,
            cancelled: false,
        }
    }
    pub fn cancel(&mut self) {
        self.cancelled = true
    }
    pub fn accept(&mut self, body: &str, tools: &ToolRuntime) -> Result<Vec<LoopEvent>, String> {
        if self.cancelled {
            return Ok(vec![LoopEvent::Cancelled]);
        }
        let turn = decode_turn(self.protocol, body)?;
        self.accept_turn(turn, tools)
    }
    pub fn accept_turn(
        &mut self,
        turn: NormalizedTurn,
        tools: &ToolRuntime,
    ) -> Result<Vec<LoopEvent>, String> {
        if self.cancelled {
            return Ok(vec![LoopEvent::Cancelled]);
        }
        if self.steps >= self.max_steps {
            return Ok(vec![LoopEvent::LimitReached {
                max_steps: self.max_steps,
            }]);
        }
        self.steps += 1;
        let mut out = turn
            .text
            .into_iter()
            .map(|text| LoopEvent::ModelText { text })
            .collect::<Vec<_>>();
        for call in turn.tool_calls {
            let prepared = tools.prepare(call.request).map_err(|e| e.detail)?;
            if prepared.approval_required {
                out.push(LoopEvent::ApprovalRequired { call: prepared });
            } else {
                out.push(LoopEvent::ToolFinished {
                    result: tools.execute(&prepared.call_id),
                });
            }
        }
        if turn.finished
            && out
                .iter()
                .all(|e| !matches!(e, LoopEvent::ApprovalRequired { .. }))
        {
            out.push(LoopEvent::Finished)
        }
        Ok(out)
    }
    pub fn resolve(&self, tools: &ToolRuntime, call_id: &str, approved: bool) -> ToolResult {
        if self.cancelled {
            let _ = tools.cancel(call_id);
            return tools.execute(call_id);
        }
        let _ = tools.resolve_approval(call_id, approved);
        tools.execute(call_id)
    }
    pub fn outcome(
        result: &ToolResult,
        provider_call_id: String,
        tool_name: String,
    ) -> ToolOutcome {
        let content = serde_json::to_string(&VisibleResult::from(result))
            .unwrap_or_else(|_| "{\"ok\":false}".into());
        ToolOutcome {
            provider_call_id,
            tool_name,
            ok: result.ok,
            content,
        }
    }
}

#[derive(Serialize)]
struct VisibleResult<'a> {
    ok: bool,
    output: &'a Option<String>,
    error: &'a Option<rex_tools::ToolError>,
    receipt: &'a AuditReceipt,
}
impl<'a> From<&'a ToolResult> for VisibleResult<'a> {
    fn from(r: &'a ToolResult) -> Self {
        Self {
            ok: r.ok,
            output: &r.output,
            error: &r.error,
            receipt: &r.receipt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{NormalizedToolCall, NormalizedTurn};
    use rex_tools::ToolRequest;
    use std::fs;

    fn runtime() -> (tempfile::TempDir, ToolRuntime) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tools = ToolRuntime::new(dir.path()).unwrap();
        (dir, tools)
    }

    #[test]
    fn clamps_steps_and_stops_at_the_bound() {
        let (_dir, tools) = runtime();
        let mut loop_ = AgentLoop::new(ProviderProtocol::OpenAiCompatible, HARD_MAX_STEPS + 99);
        let turn = NormalizedTurn {
            text: vec![],
            tool_calls: vec![],
            finished: false,
        };
        for _ in 0..HARD_MAX_STEPS {
            assert!(loop_.accept_turn(turn.clone(), &tools).unwrap().is_empty());
        }
        assert!(matches!(
            loop_.accept_turn(turn, &tools).unwrap().as_slice(),
            [LoopEvent::LimitReached {
                max_steps: HARD_MAX_STEPS
            }]
        ));
    }

    #[test]
    fn read_auto_executes_but_write_suspends_then_is_one_shot() {
        let (_dir, tools) = runtime();
        let mut loop_ = AgentLoop::new(ProviderProtocol::OpenAiCompatible, DEFAULT_MAX_STEPS);
        let read = NormalizedTurn {
            text: vec![],
            tool_calls: vec![NormalizedToolCall {
                provider_call_id: "r".into(),
                request: ToolRequest::ReadFile {
                    path: "a.txt".into(),
                },
            }],
            finished: false,
        };
        assert!(
            matches!(loop_.accept_turn(read, &tools).unwrap().as_slice(), [LoopEvent::ToolFinished { result }] if result.ok)
        );
        let write = NormalizedTurn {
            text: vec![],
            tool_calls: vec![NormalizedToolCall {
                provider_call_id: "w".into(),
                request: ToolRequest::CreateFile {
                    path: "b.txt".into(),
                    content: "x".into(),
                    overwrite: false,
                },
            }],
            finished: false,
        };
        let events = loop_.accept_turn(write, &tools).unwrap();
        let call = match &events[0] {
            LoopEvent::ApprovalRequired { call } => call,
            _ => panic!("write did not suspend"),
        };
        let first = loop_.resolve(&tools, &call.call_id, true);
        assert!(first.ok);
        let second = tools.execute(&call.call_id);
        assert!(!second.ok);
    }

    #[test]
    fn cancellation_and_denial_are_terminal() {
        let (_dir, tools) = runtime();
        let mut loop_ = AgentLoop::new(ProviderProtocol::OpenAiCompatible, 2);
        loop_.cancel();
        let turn = NormalizedTurn {
            text: vec![],
            tool_calls: vec![],
            finished: false,
        };
        assert!(matches!(
            loop_.accept_turn(turn, &tools).unwrap().as_slice(),
            [LoopEvent::Cancelled]
        ));
    }
}
