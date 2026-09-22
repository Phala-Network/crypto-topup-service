//! Coin Metrics `ReferenceRateUSD` adapter.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use topup_core::valuation::{Observation, SourceId, UnixSeconds};

use super::decimal::parse_scaled;
use super::{PriceError, PriceSource, http_client};

const ENDPOINT: &str = "https://community-api.coinmetrics.io/v4/timeseries/asset-metrics";

/// Coin Metrics asset-metric observation source.
#[derive(Debug)]
pub struct CoinMetrics {
    client: reqwest::Client,
    endpoint: Url,
    asset: String,
    metric: String,
    frequency: String,
    api_key: Option<String>,
}

impl CoinMetrics {
    /// Creates a source using the public endpoint and optional `COINMETRICS_API_KEY`.
    pub fn new(asset: String, metric: String, frequency: String) -> Result<Self, PriceError> {
        Self::with_endpoint(asset, metric, frequency, ENDPOINT)
    }

    fn with_endpoint(
        asset: String,
        metric: String,
        frequency: String,
        endpoint: &str,
    ) -> Result<Self, PriceError> {
        let endpoint = Url::parse(endpoint).map_err(|_| PriceError::InvalidUrl)?;
        let api_key = std::env::var("COINMETRICS_API_KEY")
            .ok()
            .filter(|value| !value.is_empty());
        Ok(Self {
            client: http_client()?,
            endpoint,
            asset,
            metric,
            frequency,
            api_key,
        })
    }

    fn parse_response(&self, body: &[u8]) -> Result<Observation, PriceError> {
        let response: Response =
            serde_json::from_slice(body).map_err(|_| PriceError::MalformedResponse("body"))?;
        let row = response
            .data
            .into_iter()
            .next()
            .ok_or(PriceError::MalformedResponse("data"))?;
        if row.asset != self.asset {
            return Err(PriceError::MalformedResponse("data.asset"));
        }
        let raw_price = row
            .metrics
            .get(&self.metric)
            .and_then(Value::as_str)
            .ok_or(PriceError::MalformedResponse("data.metric"))?;
        let observed_at = DateTime::parse_from_rfc3339(&row.time)
            .map_err(|_| PriceError::MalformedResponse("data.time"))?
            .with_timezone(&Utc)
            .timestamp();
        Ok(Observation {
            source: SourceId::new("coinmetrics"),
            price: parse_scaled(raw_price)?,
            observed_at: UnixSeconds::new(
                u64::try_from(observed_at).map_err(|_| PriceError::InvalidTimestamp)?,
            ),
        })
    }
}

#[async_trait]
impl PriceSource for CoinMetrics {
    async fn observe(&self) -> Result<Observation, PriceError> {
        let mut request = self.client.get(self.endpoint.clone()).query(&[
            ("assets", self.asset.as_str()),
            ("metrics", self.metric.as_str()),
            ("frequency", self.frequency.as_str()),
            ("limit_per_asset", "1"),
            ("paging_from", "end"),
        ]);
        if let Some(api_key) = &self.api_key {
            request = request.query(&[("api_key", api_key)]);
        }
        let response = request
            .send()
            .await
            .map_err(|_| PriceError::Request("coinmetrics fetch"))?;
        if !response.status().is_success() {
            return Err(PriceError::HttpStatus(response.status().as_u16()));
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| PriceError::Request("coinmetrics body"))?;
        self.parse_response(&body)
    }
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    asset: String,
    time: String,
    #[serde(flatten)]
    metrics: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_response() {
        let source = CoinMetrics::new(
            "pha".to_owned(),
            "ReferenceRateUSD".to_owned(),
            "1m".to_owned(),
        )
        .expect("source");
        let observation = source
            .parse_response(include_bytes!(
                "../../tests/fixtures/pricing/coinmetrics.json"
            ))
            .expect("fixture parses");
        assert_eq!(observation.price.value(), 12_345_678);
        assert_eq!(observation.observed_at.value(), 1_790_035_200);
    }

    #[test]
    fn rejects_recorded_malformed_response() {
        let source = CoinMetrics::new(
            "pha".to_owned(),
            "ReferenceRateUSD".to_owned(),
            "1m".to_owned(),
        )
        .expect("source");
        assert!(
            source
                .parse_response(include_bytes!(
                    "../../tests/fixtures/pricing/coinmetrics-malformed.json"
                ))
                .is_err()
        );
    }
}
