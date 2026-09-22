//! RFC 9421 HTTP Message Signatures verification shared by every inbound verifier.
//!
//! The service authenticates product and admin requests with this profile, and the settlement
//! conformance reference uses the same code to verify service-signed settlement requests. Header
//! values are parsed as RFC 8941 Structured Fields, so any signature label, any parameter order,
//! and an optional `alg="ed25519"` parameter are accepted.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use sfv::{BareItem, Dictionary, FieldType as _, List, ListEntry, Parser, Version};
use sha2::{Digest, Sha256};

/// Components every signature must cover, in order.
const REQUIRED_COMPONENTS: [&str; 3] = ["@method", "@target-uri", "content-digest"];
/// Optional fourth component, required whenever the request carries the header.
const IDEMPOTENCY_COMPONENT: &str = "idempotency-key";
/// Maximum distance between `created` and the verifier's clock.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 300;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerificationFailed;

impl std::fmt::Display for VerificationFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HTTP message signature verification failed")
    }
}

impl std::error::Error for VerificationFailed {}

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
        let base = signature_base(
            message.method,
            message.target_uri,
            message.content_digest.trim(),
            idempotency_key,
            &parsed.parameters,
        );
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

fn signature_base(
    method: &str,
    target_uri: &str,
    content_digest: &str,
    idempotency_key: Option<&str>,
    parameters: &str,
) -> String {
    let mut base = format!(
        "\"@method\": {method}\n\"@target-uri\": {target_uri}\n\"content-digest\": {content_digest}"
    );
    if let Some(idempotency_key) = idempotency_key {
        base.push_str("\n\"idempotency-key\": ");
        base.push_str(idempotency_key);
    }
    base.push_str("\n\"@signature-params\": ");
    base.push_str(parameters);
    base
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

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
        let base = signature_base(
            "POST",
            URI,
            &digest(),
            idempotency.then_some(KEY),
            &signature_params,
        );
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
}
