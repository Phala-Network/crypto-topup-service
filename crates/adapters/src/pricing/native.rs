//! Minimal Coin Metrics `ReferenceRateUSD` adapter used until C4 provides shared pricing.

use std::time::Duration;

use serde::Deserialize;
use topup_core::money::{PRICE_SCALE, ScaledPrice};

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
        parse_scaled_price(value)
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

fn parse_scaled_price(value: &str) -> Result<ScaledPrice, String> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.starts_with('-') || fraction.len() > usize::from(PRICE_SCALE) {
        return Err("Coin Metrics rate is negative or exceeds eight decimals".to_owned());
    }
    let whole = whole
        .parse::<u64>()
        .map_err(|error| format!("invalid Coin Metrics whole price: {error}"))?;
    let mut fraction_text = fraction.to_owned();
    while fraction_text.len() < usize::from(PRICE_SCALE) {
        fraction_text.push('0');
    }
    let fraction = if fraction_text.is_empty() {
        0
    } else {
        fraction_text
            .parse::<u64>()
            .map_err(|error| format!("invalid Coin Metrics fractional price: {error}"))?
    };
    let scale = 10_u64
        .checked_pow(u32::from(PRICE_SCALE))
        .ok_or_else(|| "Coin Metrics price scale overflowed".to_owned())?;
    let scaled = whole
        .checked_mul(scale)
        .and_then(|result| result.checked_add(fraction))
        .ok_or_else(|| "Coin Metrics price overflowed".to_owned())?;
    ScaledPrice::new(scaled, PRICE_SCALE).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::parse_scaled_price;

    #[test]
    fn parses_decimal_without_floating_point() {
        assert_eq!(
            parse_scaled_price("2741.49")
                .expect("price is valid")
                .value(),
            274_149_000_000
        );
        assert!(parse_scaled_price("1.123456789").is_err());
    }
}
