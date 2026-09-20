//! Custody budgets: granted ceilings and tracked consumption.
//!
//! These mirror the autonomous runtime's budgets but belong to the grant,
//! not one run: custody across a resume keeps the same consumption ledger,
//! so an agent cannot reset its budget by crashing.

use crate::state::BudgetKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyBudgets {
    pub max_steps: u64,
    pub max_tool_calls: u64,
    pub max_wall_ms: u64,
    pub max_tokens: u64,
}

impl Default for CustodyBudgets {
    fn default() -> Self {
        Self {
            max_steps: 24,
            max_tool_calls: 80,
            max_wall_ms: 20 * 60 * 1000,
            max_tokens: 250_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consumption {
    pub steps: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    pub wall_ms: u64,
}

impl CustodyBudgets {
    /// First budget this consumption exceeds, if any.
    pub fn first_exceeded(&self, used: &Consumption) -> Option<BudgetKind> {
        if used.steps > self.max_steps {
            Some(BudgetKind::Steps)
        } else if used.tool_calls > self.max_tool_calls {
            Some(BudgetKind::ToolCalls)
        } else if used.tokens > self.max_tokens {
            Some(BudgetKind::Tokens)
        } else if used.wall_ms > self.max_wall_ms {
            Some(BudgetKind::WallTime)
        } else {
            None
        }
    }
}
