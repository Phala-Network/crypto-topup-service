//! JSON-RPC calls counted per provider, chain, and method, for measuring provider usage.
//!
//! Every [`super::EvmClient`] request passes through [`CountingLayer`], so every call is counted
//! once when it is sent, whether it succeeds or not: that is what providers bill. Labels are
//! bounded: provider labels are the configured provider ids (never URLs), chains are the
//! configured chain ids, and methods outside [`METHODS`] count as `other`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::SystemTime;

use alloy::rpc::json_rpc::{RequestPacket, ResponsePacket};
use alloy::transports::{TransportError, TransportFut};
use tower::{Layer, Service};

/// Methods counted under their own name; every other method counts as `other`.
pub const METHODS: [&str; 16] = [
    "eth_blockNumber",
    "eth_call",
    "eth_chainId",
    "eth_estimateGas",
    "eth_feeHistory",
    "eth_gasPrice",
    "eth_getBalance",
    "eth_getBlockByHash",
    "eth_getBlockByNumber",
    "eth_getCode",
    "eth_getLogs",
    "eth_getTransactionByHash",
    "eth_getTransactionCount",
    "eth_getTransactionReceipt",
    "eth_maxPriorityFeePerGas",
    "eth_sendRawTransaction",
];

/// Label of a client without a configured provider id.
pub const UNLABELED_PROVIDER: &str = "unlabeled";

/// Counter key: provider label, chain id (`None` before a client is bound to a chain), method.
type CallKey = (String, Option<u64>, &'static str);

static CALLS: Mutex<BTreeMap<CallKey, u64>> = Mutex::new(BTreeMap::new());
/// When the first call was counted; rates are the counters over the time since.
static COUNTING_SINCE: OnceLock<SystemTime> = OnceLock::new();

/// When this process counted its first call, if it has.
#[must_use]
pub fn counting_since() -> Option<SystemTime> {
    COUNTING_SINCE.get().copied()
}

/// One counter: the calls sent to `provider` for `chain_id` with `method` since process start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcCallCount {
    /// Configured provider id.
    pub provider: String,
    /// Chain id, when the client is bound to one.
    pub chain_id: Option<u64>,
    /// JSON-RPC method, or `other`.
    pub method: &'static str,
    /// Calls sent.
    pub calls: u64,
}

/// Returns every counter, ordered by provider, chain, and method.
#[must_use]
pub fn rpc_call_counts() -> Vec<RpcCallCount> {
    CALLS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|((provider, chain_id, method), calls)| RpcCallCount {
            provider: provider.clone(),
            chain_id: *chain_id,
            method,
            calls: *calls,
        })
        .collect()
}

/// Returns the calls counted for one provider label, by method, for tests and measurements.
#[must_use]
pub fn provider_call_counts(provider: &str) -> BTreeMap<&'static str, u64> {
    let mut counts = BTreeMap::new();
    for count in rpc_call_counts() {
        if count.provider == provider {
            let calls: &mut u64 = counts.entry(count.method).or_default();
            *calls = calls.saturating_add(count.calls);
        }
    }
    counts
}

fn bounded_method(method: &str) -> &'static str {
    METHODS
        .iter()
        .find(|known| **known == method)
        .copied()
        .unwrap_or("other")
}

fn record(labels: &CallLabels, method: &str) {
    COUNTING_SINCE.get_or_init(SystemTime::now);
    let key = (
        labels.provider.clone(),
        labels.chain_id,
        bounded_method(method),
    );
    let mut calls = CALLS.lock().unwrap_or_else(PoisonError::into_inner);
    let count = calls.entry(key).or_default();
    *count = count.saturating_add(1);
}

/// The labels one client's calls are counted under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CallLabels {
    pub(super) provider: String,
    pub(super) chain_id: Option<u64>,
}

impl Default for CallLabels {
    fn default() -> Self {
        Self {
            provider: UNLABELED_PROVIDER.to_owned(),
            chain_id: None,
        }
    }
}

/// Counts every JSON-RPC request, including each request of a batch, under fixed labels.
#[derive(Clone, Debug)]
pub(super) struct CountingLayer {
    labels: Arc<CallLabels>,
}

impl CountingLayer {
    pub(super) fn new(labels: CallLabels) -> Self {
        Self {
            labels: Arc::new(labels),
        }
    }
}

impl<S> Layer<S> for CountingLayer {
    type Service = CountingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CountingService {
            inner,
            labels: Arc::clone(&self.labels),
        }
    }
}

/// The transport wrapped by [`CountingLayer`].
#[derive(Clone, Debug)]
pub(super) struct CountingService<S> {
    inner: S,
    labels: Arc<CallLabels>,
}

impl<S> Service<RequestPacket> for CountingService<S>
where
    S: Service<
            RequestPacket,
            Response = ResponsePacket,
            Error = TransportError,
            Future = TransportFut<'static>,
        >,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        for method in request.method_names() {
            record(&self.labels, method);
        }
        self.inner.call(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_methods_share_one_label() {
        assert_eq!(bounded_method("eth_getLogs"), "eth_getLogs");
        assert_eq!(bounded_method("debug_traceTransaction"), "other");
        assert_eq!(bounded_method("attacker-chosen-name"), "other");
    }
}
