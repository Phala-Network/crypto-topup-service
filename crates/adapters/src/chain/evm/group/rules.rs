//! Bounded reviewed provider error mappings, without a programmable policy engine.
use super::{Failure, transport::HttpReply};
use serde::{Deserialize, Serialize};
/// Only explicitly reviewable classes can be mapped from unknown provider errors.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Retry within quota/deadline bounds.
    Throttled,
    /// Shrink numeric windows in the caller; no partial commit.
    Range,
    /// Method unavailable on this member.
    Capability,
    /// Transient server fault.
    Server,
    /// Terminal invalid request.
    Request,
}
impl ErrorClass {
    pub(super) fn failure(self) -> Failure {
        match self {
            Self::Throttled => Failure::Throttled,
            Self::Range => Failure::Range,
            Self::Capability => Failure::Capability,
            Self::Server => Failure::Server,
            Self::Request => Failure::Request,
        }
    }
}
/// Scope to pause when a quota mapping is known; unknown HTTP429 always pauses the account.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// All methods and credentials sharing this account.
    #[default]
    Account,
    /// All methods using this credential.
    Key,
}
/// Public attested allowlist entry; no regex or arbitrary callback.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ErrorRule {
    /// Reviewed company identity.
    pub company: String,
    /// Exact method names.
    pub methods: Vec<String>,
    /// Exact HTTP statuses.
    pub http_statuses: Vec<u16>,
    /// Exact JSON-RPC error code.
    pub rpc_code: i64,
    /// Lowercase bounded normalized prefix.
    pub message_prefix: String,
    /// Resulting typed class.
    pub class: ErrorClass,
    /// Explicit quota scope, defaults to account.
    #[serde(default)]
    pub budget_scope: BudgetScope,
}
impl ErrorRule {
    pub(super) fn matches(&self, company: &str, method: &str, reply: &HttpReply) -> bool {
        self.company == company
            && self.methods.iter().any(|m| m == method)
            && self.http_statuses.contains(&reply.status)
            && reply
                .body
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(serde_json::Value::as_i64)
                == Some(self.rpc_code)
            && reply
                .body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|m| {
                    m.chars()
                        .take(256)
                        .collect::<String>()
                        .to_ascii_lowercase()
                        .starts_with(&self.message_prefix)
                })
    }
}
pub(super) fn validate(rules: &[ErrorRule]) -> Result<(), &'static str> {
    if rules.len() > 32 {
        return Err("too many RPC error rules");
    }
    for (i, r) in rules.iter().enumerate() {
        if r.company.is_empty()
            || r.methods.is_empty()
            || r.http_statuses.is_empty()
            || r.message_prefix.is_empty()
            || r.message_prefix.len() > 256
            || r.message_prefix != r.message_prefix.to_ascii_lowercase()
            || matches!(r.rpc_code, -32600 | -32602)
            || r.http_statuses.iter().any(|s| {
                !(200..=599).contains(s)
                    || (300..400).contains(s)
                    || matches!(s, 401 | 403 | 408 | 413)
            })
        {
            return Err("invalid RPC error rule");
        }
        for prior in rules.iter().take(i) {
            if prior.company == r.company
                && prior.rpc_code == r.rpc_code
                && prior.methods.iter().any(|m| r.methods.contains(m))
                && prior
                    .http_statuses
                    .iter()
                    .any(|s| r.http_statuses.contains(s))
                && (prior.message_prefix.starts_with(&r.message_prefix)
                    || r.message_prefix.starts_with(&prior.message_prefix))
            {
                return Err("overlapping RPC error rules");
            }
        }
    }
    Ok(())
}
