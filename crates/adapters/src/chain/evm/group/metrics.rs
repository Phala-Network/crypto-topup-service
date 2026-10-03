//! Bounded group health metrics; URLs, keys and raw errors never become labels.
use super::RpcGroup;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::{Mutex, OnceLock, PoisonError, Weak};
static GROUPS: OnceLock<Mutex<BTreeMap<String, Weak<RpcGroup>>>> = OnceLock::new();
pub(super) fn register(group: &std::sync::Arc<RpcGroup>) {
    GROUPS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(group.id.clone(), std::sync::Arc::downgrade(group));
}
/// Appends standard Prometheus health gauges for the current configured groups.
pub fn render() -> String {
    let mut out = String::from(
        "# HELP topup_rpc_group_eligible_members Serving validated members.\n# TYPE topup_rpc_group_eligible_members gauge\n# HELP topup_rpc_member_eligible Member currently serving.\n# TYPE topup_rpc_member_eligible gauge\n# HELP topup_rpc_member_quarantined Redirect or credential quarantine.\n# TYPE topup_rpc_member_quarantined gauge\n",
    );
    let Some(groups) = GROUPS.get() else {
        return out;
    };
    for group in groups
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .filter_map(Weak::upgrade)
    {
        let _ = writeln!(
            out,
            "topup_rpc_group_eligible_members{{group=\"{}\",chain_id=\"{}\"}} {}",
            group.id,
            group.chain,
            group.eligible()
        );
        let health = group.health.lock().unwrap_or_else(PoisonError::into_inner);
        for (m, h) in group.members.iter().zip(health.iter()) {
            let labels = format!(
                "group=\"{}\",chain_id=\"{}\",member=\"{}\"",
                group.id, group.chain, m.id
            );
            let _ = writeln!(
                out,
                "topup_rpc_member_eligible{{{labels}}} {}",
                u8::from(h.eligible && !h.quarantined && h.until.is_none())
            );
            let _ = writeln!(
                out,
                "topup_rpc_member_quarantined{{{labels}}} {}",
                u8::from(h.quarantined)
            );
        }
    }
    out
}
