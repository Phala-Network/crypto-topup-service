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

type EventKey = (String, u64, String, &'static str);
static EVENTS: Mutex<BTreeMap<EventKey, u64>> = Mutex::new(BTreeMap::new());
pub(super) fn event(group: &RpcGroup, index: usize, class: &'static str, value: u64) {
    if let Some(member) = group.members.get(index) {
        let mut events = EVENTS.lock().unwrap_or_else(PoisonError::into_inner);
        let total = events
            .entry((group.id.clone(), group.chain, member.id.clone(), class))
            .or_default();
        *total = total.saturating_add(value);
    }
}
/// Failure and quota-wait counters never evict idle members or include raw upstream messages.
pub fn events() -> String {
    let mut out = String::from(
        "# HELP topup_rpc_member_failures_total Rejected group member attempts by bounded class.\n# TYPE topup_rpc_member_failures_total counter\n# HELP topup_rpc_budget_wait_seconds_total Time awaiting joint account and key admission.\n# TYPE topup_rpc_budget_wait_seconds_total counter\n",
    );
    for ((group, chain, member, class), value) in
        EVENTS.lock().unwrap_or_else(PoisonError::into_inner).iter()
    {
        let labels = format!("group=\"{group}\",chain_id=\"{chain}\",member=\"{member}\"");
        if *class == "budget_wait" {
            let _ = writeln!(
                out,
                "topup_rpc_budget_wait_seconds_total{{{labels}}} {}.{:09}",
                value.checked_div(1_000_000_000).unwrap_or(0),
                value.checked_rem(1_000_000_000).unwrap_or(0)
            );
        } else {
            let _ = writeln!(
                out,
                "topup_rpc_member_failures_total{{{labels},class=\"{class}\"}} {value}"
            );
        }
    }
    out
}
