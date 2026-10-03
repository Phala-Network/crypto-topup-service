//! Prometheus text exposition of the process's counters, served to the operator by
//! `GET /v1/admin/metrics` (deploy/README.md, "Measuring RPC usage").

use std::fmt::Write as _;
use std::time::UNIX_EPOCH;

use topup_adapters::chain::evm::metrics::{counting_since, rpc_call_counts};

/// The media type of [`render`]'s output.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Renders every counter in the Prometheus text format.
#[must_use]
pub fn render() -> String {
    let mut body = String::new();
    body.push_str(
        "# HELP topup_rpc_calls_total JSON-RPC calls sent since the process started, by \
         configured provider id, chain id, and method.\n\
         # TYPE topup_rpc_calls_total counter\n",
    );
    for count in rpc_call_counts() {
        let chain = count
            .chain_id
            .map_or_else(|| "unknown".to_owned(), |chain_id| chain_id.to_string());
        let _ = writeln!(
            body,
            "topup_rpc_calls_total{{provider=\"{}\",chain_id=\"{chain}\",method=\"{}\"}} {}",
            escape(&count.provider),
            count.method,
            count.calls
        );
    }
    if let Some(seconds) = counting_since()
        .and_then(|since| since.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
    {
        body.push_str(
            "# HELP topup_rpc_calls_since_seconds Unix time of the first counted call.\n\
             # TYPE topup_rpc_calls_since_seconds gauge\n",
        );
        let _ = writeln!(body, "topup_rpc_calls_since_seconds {seconds}");
    }
    body.push_str(&topup_adapters::chain::evm::group::metrics::render());
    body.push_str(&crate::db::rpc::metrics());
    body
}

/// Escapes a label value as the text format requires.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
