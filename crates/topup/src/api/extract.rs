//! Request extractors that answer rejections with the API error object.

use axum::Json;
use axum::extract::path::ErrorKind;
use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use serde::de::DeserializeOwned;
use sfv::{BareItem, Item, Parser};

use super::error::ApiError;

/// A JSON body whose rejection is a `400` error object: `parameter_missing` or
/// `parameter_unknown` naming the field, otherwise `parameter_invalid`.
pub(crate) struct ApiJson<T>(pub(crate) T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(deserialize_error(&rejection.body_text())),
        }
    }
}

/// A query string whose rejection is the `400` error object [`ApiJson`] returns.
pub(crate) struct ApiQuery<T>(pub(crate) T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(deserialize_error(&rejection.body_text())),
        }
    }
}

/// Path parameters whose rejection is a `400` `parameter_invalid` error object naming the
/// parameter, without the parser's message.
pub(crate) struct ApiPath<T>(pub(crate) T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Self(value)),
            Err(rejection) => Err(path_error(&rejection)),
        }
    }
}

fn path_error(rejection: &PathRejection) -> ApiError {
    let PathRejection::FailedToDeserializePathParams(error) = rejection else {
        tracing::error!(%rejection, "route has no path parameters to extract");
        return ApiError::internal();
    };
    match error.kind() {
        ErrorKind::ParseErrorAtKey { key, .. }
        | ErrorKind::InvalidUtf8InPathParam { key }
        | ErrorKind::DeserializeError { key, .. } => {
            ApiError::invalid_param(key.clone(), format!("{key} is not a valid value"))
        }
        _ => ApiError::bad_request("a path parameter is not a valid value"),
    }
}

/// Maps a serde rejection of a body or query string: `parameter_missing` or `parameter_unknown`
/// naming the field, otherwise `parameter_invalid`.
fn deserialize_error(text: &str) -> ApiError {
    let field = text.split('`').nth(1).map(str::to_owned);
    let error = if text.contains("missing field") {
        ApiError::missing_param(text)
    } else if text.contains("unknown field") {
        ApiError::unknown_param(text)
    } else {
        ApiError::bad_request(text)
    };
    match field {
        Some(field) => error.with_param(field),
        None => error,
    }
}

/// The decoded `name=value` pairs of a query string, in order.
pub(crate) fn query_pairs(raw: Option<&str>) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// The `expand[]` values of a query, each of which must be one of `allowed`.
pub(crate) fn expansions(
    pairs: &[(String, String)],
    allowed: &[&'static str],
) -> Result<Vec<&'static str>, ApiError> {
    pairs
        .iter()
        .filter(|(name, _)| name == "expand[]" || name == "expand")
        .map(|(_, value)| {
            allowed
                .iter()
                .copied()
                .find(|field| field == value)
                .ok_or_else(|| {
                    ApiError::invalid_param(
                        "expand",
                        format!("expand accepts {}", allowed.join(", ")),
                    )
                })
        })
        .collect()
}

const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// The `Idempotency-Key` header, 1 to 255 visible ASCII characters: either the RFC 8941 string
/// of the IETF Idempotency-Key draft (`"8e03…"`, what the SDK sends) or the bare token Stripe
/// clients send. The key is the string's content.
pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get(IDEMPOTENCY_KEY) else {
        return Ok(None);
    };
    let invalid = || {
        ApiError::invalid_param(
            "Idempotency-Key",
            "Idempotency-Key must be 1 to 255 visible ASCII characters",
        )
    };
    let value = value.to_str().map_err(|_| invalid())?.trim();
    let key = if value.starts_with('"') {
        match Parser::new(value).parse::<Item>() {
            Ok(Item {
                bare_item: BareItem::String(key),
                params,
            }) if params.is_empty() => key.as_str().to_owned(),
            _ => return Err(invalid()),
        }
    } else {
        value.to_owned()
    };
    if (1..=255).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        && !key.trim().is_empty()
    {
        Ok(Some(key))
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> Result<Option<String>, ()> {
        let mut headers = HeaderMap::new();
        headers.insert(IDEMPOTENCY_KEY, value.parse().map_err(|_| ())?);
        idempotency_key(&headers).map_err(|_| ())
    }

    #[test]
    fn idempotency_keys_are_structured_strings_or_bare_tokens() {
        assert_eq!(key("\"k-1\""), Ok(Some("k-1".to_owned())));
        assert_eq!(key("k-1"), Ok(Some("k-1".to_owned())));
        assert_eq!(key("\"a \\\"b\""), Ok(Some("a \"b".to_owned())));
        assert_eq!(idempotency_key(&HeaderMap::new()).ok(), Some(None));
        for invalid in ["\"\"", "\"k\";p=1", "\"unterminated", &"k".repeat(256)] {
            assert_eq!(key(invalid), Err(()), "{invalid}");
        }
    }
}
