//! Minimal Coin Metrics `ReferenceRateUSD` adapter used until C4 provides shared pricing.

use std::time::Duration;

use serde::Deserialize;
use topup_core::money::ScaledPrice;

use super::decimal::parse_scaled;

const ENDPOINT: &str = "https://community-api.coinmetrics.io/v4/timeseries/asset-metrics";

/// Coin Metrics community API client for the latest USD reference rate.
#[derive(Clone, Debug)]
pub struct CoinMetricsUsdClient {
    client: reqwest::Client,
    endpoint: String,
}

impl CoinMetricsUsdClient {
    /// Creates a client with a bounded HTTP timeout.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| format!("build Coin Metrics client: {error}"))?;
        Ok(Self {
            client,
            endpoint: ENDPOINT.to_owned(),
        })
    }

    /// Reads the newest `ReferenceRateUSD` value for one Coin Metrics asset identifier.
    pub async fn price_usd(&self, asset: &str) -> Result<ScaledPrice, String> {
        if asset.trim().is_empty() {
            return Err("Coin Metrics asset must not be empty".to_owned());
        }
        let response = self
            .client
            .get(&self.endpoint)
            .query(&[
                ("assets", asset),
                ("metrics", "ReferenceRateUSD"),
                ("frequency", "1m"),
                ("page_size", "1"),
                ("paging_from", "end"),
            ])
            .send()
            .await
            .map_err(|error| format!("Coin Metrics request failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("Coin Metrics rejected request: {error}"))?;
        let body = response
            .json::<Response>()
            .await
            .map_err(|error| format!("decode Coin Metrics response: {error}"))?;
        let value = body
            .data
            .first()
            .ok_or_else(|| format!("Coin Metrics returned no rate for `{asset}`"))?
            .reference_rate_usd
            .as_str();
        parse_scaled(value).map_err(|error| format!("invalid Coin Metrics rate: {error}"))
    }
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Rate>,
}

#[derive(Deserialize)]
struct Rate {
    #[serde(rename = "ReferenceRateUSD")]
    reference_rate_usd: String,
}

#[cfg(test)]
mod tests {
    use super::parse_scaled;

    #[test]
    fn parses_twelve_decimal_rate_with_half_even_rounding() {
        assert_eq!(
            parse_scaled("2741.123456785000")
                .expect("price is valid")
                .value(),
            274_112_345_678
        );
        assert_eq!(
            parse_scaled("2741.123456795000")
                .expect("price is valid")
                .value(),
            274_112_345_680
        );
    }
}
