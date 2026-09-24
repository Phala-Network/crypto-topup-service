//! Opt-in dstack 0.5.9 simulator coverage.

use std::num::NonZeroU32;

use dstack_sdk::dstack_client::DstackClient;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use topup_adapters::attestation::{AttestedOperator, DstackAttestor, OperatorKey, report_data};
use topup_adapters::signer::dstack::DstackSigner;
use topup_core::{DB_APP_KEY_DOMAIN, DB_OWNER_KEY_DOMAIN, Signer as _};

#[tokio::test]
async fn dstack_signing_and_attestation() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(endpoint) = std::env::var("DSTACK_SIMULATOR_ENDPOINT") else {
        eprintln!("skipped: set DSTACK_SIMULATOR_ENDPOINT to run the dstack simulator test");
        return Ok(());
    };

    let signer = DstackSigner::with_endpoint(endpoint.clone());
    let payload = b"simulator settlement payload";
    let public_key = signer.settlement_public_key().await?;
    let signature = signer.sign_settlement(payload).await?;
    let verifying_key = VerifyingKey::from_bytes(&public_key.0)?;
    verifying_key.verify(payload, &Signature::from_bytes(&signature.0))?;

    let rotated = DstackSigner::with_endpoint(endpoint.clone())
        .with_operator_key_version(NonZeroU32::new(2).ok_or("two is non-zero")?);
    let v1 = signer.operator_address().await?;
    let v2 = rotated.operator_address().await?;
    assert_ne!(v1, v2);
    let raw = DstackClient::new(Some(&endpoint))
        .get_key(Some("operator/v2".to_owned()), None)
        .await?
        .decode_key()?;
    assert_eq!(
        alloy_signer_local::PrivateKeySigner::from_slice(&raw)?.address(),
        v2
    );
    assert_eq!(rotated.settlement_public_key().await?, public_key);

    // Database passwords: stable per app id, one per login, and distinct from other domains.
    let owner = signer.derive_secret(DB_OWNER_KEY_DOMAIN).await?;
    let owner_again = DstackSigner::with_endpoint(endpoint.clone())
        .derive_secret(DB_OWNER_KEY_DOMAIN)
        .await?;
    let app = signer.derive_secret(DB_APP_KEY_DOMAIN).await?;
    let backup = signer.derive_backup_key().await?;
    assert_eq!(owner.expose_secret(), owner_again.expose_secret());
    assert_ne!(owner.expose_secret(), app.expose_secret());
    assert_ne!(app.expose_secret(), backup.expose_secret());

    let two = NonZeroU32::new(2).ok_or("two is non-zero")?;
    let operator_keys = [
        OperatorKey {
            chain_id: 11_155_111,
            key_version: NonZeroU32::MIN,
        },
        OperatorKey {
            chain_id: 1,
            key_version: two,
        },
    ];
    let evidence = DstackAttestor::with_endpoint(endpoint)
        .attest(b"integration-test", &operator_keys)
        .await?;
    assert_eq!(evidence.settlement_public_key, public_key);
    // The attestation is msgpack whose byte fields are integer arrays; the TDX quote in it carries
    // report_data zero-padded to 64 bytes.
    let mut padded = evidence.report_data.to_vec();
    padded.resize(64, 0);
    let encoded = msgpack_uints(&padded);
    assert!(
        evidence
            .quote
            .windows(encoded.len())
            .any(|window| window == encoded)
    );
    assert_eq!(
        evidence.operators,
        [
            AttestedOperator {
                chain_id: 1,
                key_version: two,
                address: v2,
            },
            AttestedOperator {
                chain_id: 11_155_111,
                key_version: NonZeroU32::MIN,
                address: v1,
            },
        ]
    );
    assert_eq!(
        evidence.report_data,
        report_data(b"integration-test", &public_key, &evidence.operators)
    );
    assert!(!evidence.info.app_id.is_empty());
    assert_eq!(evidence.info.compose_hash.len(), 32);
    Ok(())
}

/// Encodes each byte as a msgpack unsigned integer: a positive fixint below 0x80, else `0xcc b`.
fn msgpack_uints(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .flat_map(|&byte| {
            if byte < 0x80 {
                vec![byte]
            } else {
                vec![0xcc, byte]
            }
        })
        .collect()
}
