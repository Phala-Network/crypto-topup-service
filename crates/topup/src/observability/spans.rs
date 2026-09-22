use serde_json::Value;
use tracing::{Span, field};

use crate::db::Deposit;

/// Creates the required span for one claimed deposit step.
#[must_use]
pub fn deposit_step_span(deposit: &Deposit) -> Span {
    let route = deposit.route.as_deref().unwrap_or_default();
    tracing::info_span!(
        "pump.step",
        deposit_id = %deposit.id,
        chain = deposit.chain_id,
        state = ?deposit.state,
        attempt = deposit.attempt,
        route,
    )
}

/// Creates a span for one inclusive scanner window.
#[must_use]
pub fn scanner_window_span(chain: u64, from_block: u64, to_block: u64) -> Span {
    tracing::info_span!(
        "scanner.window",
        chain,
        from_block,
        to_block,
        deposit_id = field::Empty,
        state = field::Empty,
        attempt = field::Empty,
    )
}

/// Creates a span for one outbox delivery attempt.
#[must_use]
pub fn outbox_delivery_span(event_id: uuid::Uuid, payload: &Value, attempt: i32) -> Span {
    let deposit_id = payload
        .get("deposit_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let chain = payload.get("chain").and_then(Value::as_u64);
    let state = payload
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let route = payload
        .get("route")
        .and_then(Value::as_str)
        .unwrap_or_default();
    tracing::info_span!(
        "outbox.delivery",
        event_id = %event_id,
        deposit_id,
        chain = ?chain,
        state,
        attempt,
        route,
    )
}

/// Creates the span convention consumed by the C7 flusher when it lands.
#[must_use]
pub fn flush_action_span(chain: u64, route: &str, attempt: u32) -> Span {
    tracing::info_span!(
        "flush.action",
        chain,
        route,
        attempt,
        deposit_id = field::Empty,
        state = field::Empty,
    )
}

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use super::deposit_step_span;
    use crate::db::Deposit;
    use alloy_primitives::{Address, B256};
    use chrono::Utc;
    use topup_core::deposit::DepositState;
    use topup_core::money::AtomicAmount;
    use uuid::Uuid;

    #[traced_test]
    #[test]
    fn pump_step_log_carries_required_span_fields() {
        let deposit = Deposit {
            id: Uuid::parse_str("018f47f0-a9b2-7c31-8fa5-776a08f65201").expect("fixture UUID"),
            chain_id: 1,
            tx_hash: B256::ZERO,
            log_index: 0,
            block_number: 1,
            block_hash: B256::ZERO,
            block_time: Utc::now(),
            address_id: Uuid::nil(),
            account_id: Uuid::nil(),
            route: Some("route-a".to_owned()),
            route_version: Some(1),
            asset_contract: Address::ZERO,
            from_address: Address::ZERO,
            amount_atomic: AtomicAmount::new(alloy_primitives::U256::ZERO),
            state: DepositState::Confirmed,
            reason: None,
            attempt: 3,
            next_attempt_at: Utc::now(),
            lease_token: None,
            lease_until: None,
            valuation_at: None,
            price_scaled: None,
            price_source: None,
            credit_minor: None,
            quote: None,
            flush_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let span = deposit_step_span(&deposit);
        let _guard = span.enter();
        tracing::info!("pump step test event");

        assert!(logs_contain(
            "deposit_id=018f47f0-a9b2-7c31-8fa5-776a08f65201"
        ));
        assert!(logs_contain("chain=1"));
        assert!(logs_contain("state=Confirmed"));
        assert!(logs_contain("attempt=3"));
        assert!(logs_contain("route=\"route-a\""));
    }
}
