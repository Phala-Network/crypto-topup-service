//! RFC 9421 HTTP Message Signatures: the one signing and verification profile.
//!
//! The settlement client signs with [`sign`]; the service authenticates product and admin
//! requests with [`verify`], and the settlement conformance reference uses it to verify
//! service-signed settlement requests. Both build the same signature base, and every header
//! value is serialized or parsed as an RFC 8941 Structured Field, so a verifier accepts any
//! signature label, any parameter order, and an optional `alg="ed25519"` parameter.
//!
//! A verifier reconstructs `@target-uri` (RFC 9421 section 2.2.2) from its own configured
//! [`PublicOrigin`] and the request's path and query, never from `Host` or `X-Forwarded-*`
//! headers: behind a TLS-terminating gateway those describe the internal hop, not the URI the
//! signer addressed.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use sfv::{
    BareItem, Dictionary, FieldType as _, ItemSerializer, List, ListEntry, Parser, StringRef,
    Version,
};
use sha2::{Digest, Sha256};
use topup_core::{Signer, SignerError};

/// Components every signature must cover, in order.
const REQUIRED_COMPONENTS: [&str; 3] = ["@method", "@target-uri", "content-digest"];
/// Optional fourth component, required whenever the request carries the header.
const IDEMPOTENCY_COMPONENT: &str = "idempotency-key";
/// Maximum distance between `created` and the verifier's clock.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 300;
/// Label of every signature this service produces.
const SIGNATURE_LABEL: &str = "sig1";

/// The scheme and authority a verifier is publicly reachable at, such as
/// `https://topup.example`.
///
/// Parsing accepts `http` and `https` with a host and optional port, and rejects user
/// information, a path other than `/`, a query, and a fragment. The scheme and host are
/// lowercased and a default port is dropped, matching the URI an HTTP client sends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicOrigin(String);

/// Why a configured public origin was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidPublicOrigin(&'static str);

impl PublicOrigin {
    /// Parses and normalizes a configured origin.
    pub fn parse(value: &str) -> Result<Self, InvalidPublicOrigin> {
        let url = url::Url::parse(value)
            .map_err(|_| InvalidPublicOrigin("public origin must be an absolute URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(InvalidPublicOrigin(
                "public origin scheme must be http or https",
            ));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(InvalidPublicOrigin("public origin must include a host"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(InvalidPublicOrigin(
                "public origin must not include user information",
            ));
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(InvalidPublicOrigin(
                "public origin must not include a path, query, or fragment",
            ));
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    /// Returns the `@target-uri` for a request whose target has `path_and_query`, the raw
    /// origin-form path and optional query as received.
    pub fn target_uri(&self, path_and_query: &str) -> String {
        format!("{}{path_and_query}", self.0)
    }
}

impl std::fmt::Display for PublicOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The signed parts of one inbound HTTP request.
#[derive(Clone, Copy, Debug)]
pub struct SignedMessage<'a> {
    /// HTTP method, for example `POST`.
    pub method: &'a str,
    /// Absolute target URI as seen by the signer.
    pub target_uri: &'a str,
    /// Raw `Content-Digest` header value.
    pub content_digest: &'a str,
    /// Raw `Idempotency-Key` header value, when present.
    pub idempotency_key: Option<&'a str>,
    /// Raw `Signature-Input` header value.
    pub signature_input: &'a str,
    /// Raw `Signature` header value.
    pub signature: &'a str,
    /// Exact request body bytes.
    pub body: &'a [u8],
}

/// A signature which verified under the configured key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSignature {
    /// Key identifier from the verified signature parameters.
    pub keyid: String,
    /// Unix `created` timestamp from the verified signature parameters.
    pub created: i64,
    /// SHA-256 of the signature bytes, used for single-use tracking.
    pub signature_hash: [u8; 32],
    /// Whether the verified signature covered `idempotency-key`.
    pub covers_idempotency_key: bool,
}

/// Opaque verification failure; callers answer `401` without detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("HTTP message signature verification failed")]
pub struct VerificationFailed;

/// The covered components of one outbound request, as their header values will be sent.
#[derive(Clone, Copy, Debug)]
pub struct Components<'a> {
    /// HTTP method, for example `POST`.
    pub method: &'a str,
    /// Absolute target URI.
    pub target_uri: &'a str,
    /// `Content-Digest` header value, as produced by [`content_digest`].
    pub content_digest: &'a str,
    /// `Idempotency-Key` header value, as produced by [`structured_string`], when covered.
    pub idempotency_key: Option<&'a str>,
}

impl<'a> Components<'a> {
    /// Returns the covered `(identifier, value)` pairs in profile order.
    fn lines(&self) -> Vec<(&'static str, &'a str)> {
        let mut lines = REQUIRED_COMPONENTS
            .into_iter()
            .zip([self.method, self.target_uri, self.content_digest])
            .collect::<Vec<_>>();
        if let Some(idempotency_key) = self.idempotency_key {
            lines.push((IDEMPOTENCY_COMPONENT, idempotency_key));
        }
        lines
    }
}

/// Returns the `Content-Digest` header value of `body`.
#[must_use]
pub fn content_digest(body: &[u8]) -> String {
    format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body)))
}

/// Serializes `value` as an RFC 8941 String, or `None` when it is not representable.
#[must_use]
pub fn structured_string(value: &str) -> Option<String> {
    let value = StringRef::from_str(value).ok()?;
    Some(ItemSerializer::new().bare_item(value).finish())
}

/// Signs `components` as `keyid` at Unix time `created` with the settlement key.
///
/// Returns the `Signature-Input` and `Signature` header values. `keyid` must be an RFC 8941
/// String without `"` or `\`, as `SettlementClient` checks when it is built.
pub async fn sign<S: Signer>(
    signer: &S,
    components: &Components<'_>,
    created: i64,
    keyid: &str,
) -> Result<(String, String), SignerError> {
    let lines = components.lines();
    let covered = lines
        .iter()
        .map(|(identifier, _)| format!("\"{identifier}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let parameters = format!("({covered});created={created};keyid=\"{keyid}\"");
    let signature = signer
        .sign_settlement(signature_base(&lines, &parameters).as_bytes())
        .await?;
    Ok((
        format!("{SIGNATURE_LABEL}={parameters}"),
        format!("{SIGNATURE_LABEL}=:{}:", STANDARD.encode(signature.0)),
    ))
}

/// Verifies one request against a pinned `(keyid, public key)` pair at Unix time `now`.
///
/// Every dictionary member of `Signature-Input` is tried; the first entry which covers exactly
/// the profile components, names `keyid`, is fresh, and verifies returns success. When the
/// request carries `Idempotency-Key`, the verified signature must cover it.
pub fn verify(
    message: &SignedMessage<'_>,
    keyid: &str,
    key: &VerifyingKey,
    now: i64,
) -> Result<VerifiedSignature, VerificationFailed> {
    verify_content_digest(message.content_digest, message.body)?;
    let signature_inputs = parse_dictionary(message.signature_input)?;
    let signatures = parse_dictionary(message.signature)?;
    let idempotency_key = message.idempotency_key.map(str::trim);

    for (label, entry) in &signature_inputs {
        let Ok(parsed) = parse_signature_input_entry(entry) else {
            continue;
        };
        if parsed.covers_idempotency_key != idempotency_key.is_some() {
            continue;
        }
        if parsed.keyid != keyid || now.abs_diff(parsed.created) > MAX_CLOCK_SKEW_SECONDS {
            continue;
        }
        let Some(signature_bytes) = signature_bytes(signatures.get(label.as_str())) else {
            continue;
        };
        let Ok(signature) = Signature::from_slice(signature_bytes) else {
            continue;
        };
        let components = Components {
            method: message.method,
            target_uri: message.target_uri,
            content_digest: message.content_digest.trim(),
            idempotency_key,
        };
        let base = signature_base(&components.lines(), &parsed.parameters);
        if key.verify_strict(base.as_bytes(), &signature).is_ok() {
            return Ok(VerifiedSignature {
                keyid: parsed.keyid,
                created: parsed.created,
                signature_hash: Sha256::digest(signature_bytes).into(),
                covers_idempotency_key: parsed.covers_idempotency_key,
            });
        }
    }
    Err(VerificationFailed)
}

fn verify_content_digest(value: &str, body: &[u8]) -> Result<(), VerificationFailed> {
    let encoded = value
        .trim()
        .strip_prefix("sha-256=:")
        .and_then(|value| value.strip_suffix(':'))
        .ok_or(VerificationFailed)?;
    let supplied = STANDARD.decode(encoded).map_err(|_| VerificationFailed)?;
    let expected: [u8; 32] = Sha256::digest(body).into();
    if supplied.as_slice() == expected {
        Ok(())
    } else {
        Err(VerificationFailed)
    }
}

struct ParsedSignatureInput {
    created: i64,
    keyid: String,
    parameters: String,
    covers_idempotency_key: bool,
}

fn parse_dictionary(value: &str) -> Result<Dictionary, VerificationFailed> {
    Parser::new(value.trim())
        .with_version(Version::Rfc8941)
        .parse()
        .map_err(|_| VerificationFailed)
}

fn parse_signature_input_entry(
    entry: &ListEntry,
) -> Result<ParsedSignatureInput, VerificationFailed> {
    let ListEntry::InnerList(inner_list) = entry else {
        return Err(VerificationFailed);
    };
    let covers_idempotency_key = match inner_list.items.len() {
        3 => false,
        4 => true,
        _ => return Err(VerificationFailed),
    };
    let expected = REQUIRED_COMPONENTS
        .iter()
        .chain(covers_idempotency_key.then_some(&IDEMPOTENCY_COMPONENT));
    for (item, expected) in inner_list.items.iter().zip(expected) {
        if !item.params.is_empty() {
            return Err(VerificationFailed);
        }
        let BareItem::String(component) = &item.bare_item else {
            return Err(VerificationFailed);
        };
        if component.as_str() != *expected {
            return Err(VerificationFailed);
        }
    }

    let created = match inner_list.params.get("created") {
        Some(BareItem::Integer(created)) => (*created).into(),
        _ => return Err(VerificationFailed),
    };
    let keyid = match inner_list.params.get("keyid") {
        Some(BareItem::String(keyid)) if !keyid.as_str().is_empty() => keyid.as_str().to_owned(),
        _ => return Err(VerificationFailed),
    };
    match inner_list.params.get("alg") {
        None => {}
        Some(BareItem::String(algorithm)) if algorithm.as_str() == "ed25519" => {}
        Some(_) => return Err(VerificationFailed),
    }

    let parameters = List::from([entry.clone()])
        .serialize()
        .ok_or(VerificationFailed)?;
    Ok(ParsedSignatureInput {
        created,
        keyid,
        parameters,
        covers_idempotency_key,
    })
}

fn signature_bytes(entry: Option<&ListEntry>) -> Option<&[u8]> {
    let ListEntry::Item(item) = entry? else {
        return None;
    };
    if !item.params.is_empty() {
        return None;
    }
    let BareItem::ByteSequence(bytes) = &item.bare_item else {
        return None;
    };
    Some(bytes)
}

/// Formats the RFC 9421 section 2.5 signature base; signing and verification both use it.
fn signature_base(components: &[(&str, &str)], parameters: &str) -> String {
    let mut base = String::new();
    for (identifier, value) in components {
        base.push('"');
        base.push_str(identifier);
        base.push_str("\": ");
        base.push_str(value);
        base.push('\n');
    }
    base.push_str("\"@signature-params\": ");
    base.push_str(parameters);
    base
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::{Signer as _, SigningKey};
    use topup_core::SecretKey32;

    use super::*;
    use crate::signer::DevSigner;

    #[test]
    fn public_origin_is_normalized_and_validated() {
        for (input, expected) in [
            ("https://topup.example", "https://topup.example"),
            ("https://Topup.Example/", "https://topup.example"),
            ("https://topup.example:443", "https://topup.example"),
            ("http://127.0.0.1:18080", "http://127.0.0.1:18080"),
            ("https://topup.example:8443", "https://topup.example:8443"),
        ] {
            assert_eq!(
                PublicOrigin::parse(input).map(|origin| origin.to_string()),
                Ok(expected.to_owned()),
                "{input}"
            );
        }
        for input in [
            "",
            "topup.example",
            "/v1",
            "ftp://topup.example",
            "https://user@topup.example",
            "https://topup.example/api",
            "https://topup.example/?a=1",
            "https://topup.example/#top",
        ] {
            assert!(PublicOrigin::parse(input).is_err(), "{input}");
        }
        assert_eq!(
            PublicOrigin::parse("https://topup.example")
                .map(|origin| origin.target_uri("/v1/products/acme/accounts?x=1")),
            Ok("https://topup.example/v1/products/acme/accounts?x=1".to_owned())
        );
    }

    const NOW: i64 = 1_800_000_000;
    const URI: &str = "https://product.example/settlements";
    const KEY: &str = "\"deposit:abc\"";
    const BODY: &[u8] = b"{\"version\":1}";

    fn digest() -> String {
        format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(BODY)))
    }

    struct Signed {
        input: String,
        signature: String,
    }

    /// Signs `components` with parameters serialized exactly as `parameters`.
    fn sign(label: &str, components: &str, parameters: &str, idempotency: bool) -> Signed {
        let key = SigningKey::from_bytes(&[5; 32]);
        let signature_params = format!("({components}){parameters}");
        let digest = digest();
        let components = Components {
            method: "POST",
            target_uri: URI,
            content_digest: &digest,
            idempotency_key: idempotency.then_some(KEY),
        };
        let base = signature_base(&components.lines(), &signature_params);
        let signature = STANDARD.encode(key.sign(base.as_bytes()).to_bytes());
        Signed {
            input: format!("{label}={signature_params}"),
            signature: format!("{label}=:{signature}:"),
        }
    }

    fn check(signed: &Signed, idempotency: bool) -> Result<VerifiedSignature, VerificationFailed> {
        let digest = digest();
        verify(
            &SignedMessage {
                method: "POST",
                target_uri: URI,
                content_digest: &digest,
                idempotency_key: idempotency.then_some(KEY),
                signature_input: &signed.input,
                signature: &signed.signature,
                body: BODY,
            },
            "settlement/v1",
            &SigningKey::from_bytes(&[5; 32]).verifying_key(),
            NOW,
        )
    }

    const SETTLEMENT_COMPONENTS: &str =
        "\"@method\" \"@target-uri\" \"content-digest\" \"idempotency-key\"";

    #[test]
    fn signature_input_requires_the_exact_covered_components() {
        let parse = |input: &str| {
            let dictionary = parse_dictionary(input).expect("valid structured field");
            let entry = dictionary.get("sig1").expect("sig1 member");
            parse_signature_input_entry(entry).map(|parsed| parsed.covers_idempotency_key)
        };
        let parameters = ";created=1;keyid=\"product/v1\"";
        assert_eq!(
            parse(&format!(
                "sig1=(\"@method\" \"@target-uri\" \"content-digest\"){parameters}"
            )),
            Ok(false)
        );
        assert_eq!(
            parse(&format!("sig1=({SETTLEMENT_COMPONENTS}){parameters}")),
            Ok(true)
        );
        for rejected in [
            "(\"@method\" \"@target-uri\")",
            "(\"@method\" \"@target-uri\" \"content-digest\" \"x-extra\")",
            "(\"@method\";req \"@target-uri\" \"content-digest\")",
            "(\"@method\" \"@target-uri\" \"content-digest\";sf \"idempotency-key\")",
        ] {
            assert_eq!(
                parse(&format!("sig1={rejected}{parameters}")),
                Err(VerificationFailed),
                "{rejected} must be rejected"
            );
        }
    }

    #[test]
    fn accepts_the_settlement_client_profile() {
        let signed = sign(
            "sig1",
            SETTLEMENT_COMPONENTS,
            &format!(";created={NOW};keyid=\"settlement/v1\""),
            true,
        );
        let verified = check(&signed, true).expect("profile verifies");
        assert!(verified.covers_idempotency_key);
        assert_eq!(verified.created, NOW);
    }

    #[test]
    fn accepts_any_label_reordered_parameters_and_optional_alg() {
        for (label, parameters) in [
            ("product", format!(";created={NOW};keyid=\"settlement/v1\"")),
            ("x", format!(";keyid=\"settlement/v1\";created={NOW}")),
            (
                "sig2",
                format!(";alg=\"ed25519\";keyid=\"settlement/v1\";created={NOW}"),
            ),
            (
                "sig1",
                format!(";created={NOW};keyid=\"settlement/v1\";alg=\"ed25519\""),
            ),
        ] {
            let signed = sign(label, SETTLEMENT_COMPONENTS, &parameters, true);
            assert!(
                check(&signed, true).is_ok(),
                "{label} with {parameters} must verify"
            );
        }
    }

    #[test]
    fn finds_the_matching_member_among_several_signatures() {
        let other = sign(
            "other",
            SETTLEMENT_COMPONENTS,
            &format!(";created={NOW};keyid=\"other/v1\""),
            true,
        );
        let ours = sign(
            "ours",
            SETTLEMENT_COMPONENTS,
            &format!(";created={NOW};keyid=\"settlement/v1\""),
            true,
        );
        let combined = Signed {
            input: format!("{}, {}", other.input, ours.input),
            signature: format!("{}, {}", other.signature, ours.signature),
        };
        assert!(check(&combined, true).is_ok());
    }

    #[test]
    fn rejects_wrong_alg_keyid_skew_and_idempotency_coverage() {
        let parameters = [
            format!(";created={NOW};keyid=\"settlement/v1\";alg=\"rsa-pss-sha512\""),
            format!(";created={NOW};keyid=\"settlement/v2\""),
            format!(";created={};keyid=\"settlement/v1\"", NOW - 301),
            ";keyid=\"settlement/v1\"".to_owned(),
        ];
        for parameters in parameters {
            let signed = sign("sig1", SETTLEMENT_COMPONENTS, &parameters, true);
            assert!(check(&signed, true).is_err(), "{parameters} must fail");
        }

        let uncovered = sign(
            "sig1",
            "\"@method\" \"@target-uri\" \"content-digest\"",
            &format!(";created={NOW};keyid=\"settlement/v1\""),
            false,
        );
        assert!(check(&uncovered, false).is_ok());
        assert!(
            check(&uncovered, true).is_err(),
            "a present Idempotency-Key must be covered"
        );

        let reordered_components = sign(
            "sig1",
            "\"@target-uri\" \"@method\" \"content-digest\" \"idempotency-key\"",
            &format!(";created={NOW};keyid=\"settlement/v1\""),
            true,
        );
        assert!(check(&reordered_components, true).is_err());
    }

    #[test]
    fn rejects_a_tampered_body() {
        let signed = sign(
            "sig1",
            SETTLEMENT_COMPONENTS,
            &format!(";created={NOW};keyid=\"settlement/v1\""),
            true,
        );
        let digest = digest();
        let result = verify(
            &SignedMessage {
                method: "POST",
                target_uri: URI,
                content_digest: &digest,
                idempotency_key: Some(KEY),
                signature_input: &signed.input,
                signature: &signed.signature,
                body: b"{\"version\":2}",
            },
            "settlement/v1",
            &SigningKey::from_bytes(&[5; 32]).verifying_key(),
            NOW,
        );
        assert_eq!(result, Err(VerificationFailed));
    }

    #[tokio::test]
    async fn signed_headers_have_the_profile_shape_and_verify() {
        let digest = content_digest(BODY);
        assert_eq!(digest, self::digest());
        let key = structured_string("deposit:abc").expect("key is an SFV string");
        assert_eq!(key, KEY);
        for idempotency in [true, false] {
            let (signature_input, signature) = super::sign(
                &DevSigner::new(SecretKey32::new([1; 32]), SecretKey32::new([5; 32])),
                &Components {
                    method: "POST",
                    target_uri: URI,
                    content_digest: &digest,
                    idempotency_key: idempotency.then_some(KEY),
                },
                NOW,
                "settlement/v1",
            )
            .await
            .expect("request signs");
            let components = if idempotency {
                SETTLEMENT_COMPONENTS
            } else {
                "\"@method\" \"@target-uri\" \"content-digest\""
            };
            assert_eq!(
                signature_input,
                format!("sig1=({components});created={NOW};keyid=\"settlement/v1\"")
            );
            let signed = Signed {
                input: signature_input,
                signature,
            };
            assert!(check(&signed, idempotency).is_ok());
        }
    }

    #[test]
    fn settlement_profile_signature_base_has_exact_components() {
        assert_eq!(
            signature_base(
                &Components {
                    method: "POST",
                    target_uri: "https://product.example/settlements",
                    content_digest: "sha-256=:YWJj:",
                    idempotency_key: Some("\"deposit:123\""),
                }
                .lines(),
                "(\"@method\" \"@target-uri\" \"content-digest\" \"idempotency-key\");created=1618884473;keyid=\"settlement/v1\"",
            ),
            concat!(
                "\"@method\": POST\n",
                "\"@target-uri\": https://product.example/settlements\n",
                "\"content-digest\": sha-256=:YWJj:\n",
                "\"idempotency-key\": \"deposit:123\"\n",
                "\"@signature-params\": (\"@method\" \"@target-uri\" ",
                "\"content-digest\" \"idempotency-key\");created=1618884473;",
                "keyid=\"settlement/v1\""
            )
        );
    }

    #[test]
    fn rfc_9421_ed25519_example_matches_base_and_signature() {
        let components = [
            ("date", "Tue, 20 Apr 2021 02:07:55 GMT"),
            ("@method", "POST"),
            ("@path", "/foo"),
            ("@authority", "example.com"),
            ("content-type", "application/json"),
            ("content-length", "18"),
        ];
        let parameters = concat!(
            "(\"date\" \"@method\" \"@path\" \"@authority\" ",
            "\"content-type\" \"content-length\");created=1618884473;",
            "keyid=\"test-key-ed25519\""
        );
        let base = signature_base(&components, parameters);
        let private = URL_SAFE_NO_PAD
            .decode("n4Ni-HpISpVObnQMW0wOhCKROaIKqKtW_2ZYb2p9KcU")
            .expect("RFC key is valid base64url");
        let private: [u8; 32] = private.try_into().expect("RFC key is 32 bytes");
        let key = SigningKey::from_bytes(&private);
        let signature = key.sign(base.as_bytes());
        assert_eq!(
            STANDARD.encode(signature.to_bytes()),
            concat!(
                "wqcAqbmYJ2ji2glfAMaRy4gruYYnx2nEFN2HN6jrnDnQCK1",
                "u02Gb04v9EDgwUPiu4A0w6vuQv5lIp5WPpBKRCw=="
            )
        );
        assert!(
            key.verifying_key()
                .verify_strict(base.as_bytes(), &signature)
                .is_ok()
        );
    }

    #[test]
    fn idempotency_key_is_a_quoted_structured_field_string() {
        assert_eq!(
            structured_string("deposit:123").expect("key is valid"),
            "\"deposit:123\""
        );
        assert_eq!(
            structured_string("quoted\"slash\\").expect("key is escapable"),
            "\"quoted\\\"slash\\\\\""
        );
        assert!(structured_string("line\nbreak").is_none());
    }
}
