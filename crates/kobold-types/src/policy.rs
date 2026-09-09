use serde::{Deserialize, Serialize};

use crate::tool::ToolCall;

/// The decision produced by an approval policy for a requested tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ApprovalAction {
    /// Proceed with tool execution automatically.
    Approve,

    /// Deny tool execution with a stated reason.
    Deny { reason: String },

    /// Pause execution and request interactive confirmation from the user.
    NeedUserApproval { reason: String },
}

/// Pluggable policy interface governing tool execution permissions.
#[async_trait::async_trait]
pub trait ApprovalPolicy: Send + Sync {
    /// Evaluate whether a tool call may proceed, is denied, or requires human confirmation.
    async fn check(&self, call: &ToolCall) -> ApprovalAction;
}

/// A policy that approves all tool executions unconditionally.
#[derive(Debug, Clone, Default)]
pub struct AllowAllPolicy;

#[async_trait::async_trait]
impl ApprovalPolicy for AllowAllPolicy {
    async fn check(&self, _call: &ToolCall) -> ApprovalAction {
        ApprovalAction::Approve
    }
}

/// A policy that denies all tool executions unconditionally.
#[derive(Debug, Clone, Default)]
pub struct DenyAllPolicy {
    pub reason: String,
}

impl DenyAllPolicy {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait::async_trait]
impl ApprovalPolicy for DenyAllPolicy {
    async fn check(&self, _call: &ToolCall) -> ApprovalAction {
        ApprovalAction::Deny {
            reason: if self.reason.is_empty() {
                "execution denied by policy".to_string()
            } else {
                self.reason.clone()
            },
        }
    }
}
