//! Binance public ticker-price adapter.

use async_trait::async_trait;
use reqwest::Url;
use serde::Deserialize;
use topup_core::valuation::{Observation, SourceId};

use super::decimal::parse_scaled;
use super::{PriceError, PriceSource, http_client, unix_now};

const ENDPOINT: &str = "https://data-api.binance.vision/api/v3/ticker/price";

/// Binance symbol ticker observation source.
#[derive(Debug)]
pub struct Binance {
    client: reqwest::Client,
    endpoint: Url,
    symbol: String,
}

impl Binance {
    /// Creates a source for one Binance market symbol.
    pub fn new(symbol: String) -> Result<Self, PriceError> {
        Ok(Self {
            client: http_client()?,
            endpoint: Url::parse(ENDPOINT).map_err(|_| PriceError::InvalidUrl)?,
            symbol,
        })
    }

    fn parse_response(&self, body: &[u8]) -> Result<Observation, PriceError> {
        let response: Response =
            serde_json::from_slice(body).map_err(|_| PriceError::MalformedResponse("body"))?;
        if response.symbol != self.symbol {
            return Err(PriceError::MalformedResponse("symbol"));
        }
        Ok(Observation {
            source: SourceId::new("binance"),
            price: parse_scaled(&response.price)?,
            observed_at: unix_now()?,
        })
    }
}

#[async_trait]
impl PriceSource for Binance {
    async fn observe(&self) -> Result<Observation, PriceError> {
        let response = self
            .client
            .get(self.endpoint.clone())
            .query(&[("symbol", self.symbol.as_str())])
            .send()
            .await
            .map_err(|_| PriceError::Request("binance fetch"))?;
        if !response.status().is_success() {
            return Err(PriceError::HttpStatus(response.status().as_u16()));
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| PriceError::Request("binance body"))?;
        self.parse_response(&body)
    }
}

#[derive(Deserialize)]
struct Response {
    symbol: String,
    price: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_response() {
        let source = Binance::new("PHAUSDT".to_owned()).expect("source");
        let observation = source
            .parse_response(include_bytes!("../../tests/fixtures/pricing/binance.json"))
            .expect("fixture parses");
        assert_eq!(observation.price.value(), 12_345_678);
    }

    #[test]
    fn rejects_recorded_malformed_response() {
        let source = Binance::new("PHAUSDT".to_owned()).expect("source");
        assert!(
            source
                .parse_response(include_bytes!(
                    "../../tests/fixtures/pricing/binance-malformed.json"
                ))
                .is_err()
        );
    }
}
