//! Opt-in dstack v1 simulator coverage.

use std::num::NonZeroU32;

use dstack_sdk::DstackClient;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use topup_adapters::attestation::DstackAttestor;
use topup_adapters::signer::dstack::DstackSigner;
use topup_core::{DB_APP_KEY_DOMAIN, DB_OWNER_KEY_DOMAIN, Signer as _};

#[tokio::test]
async fn dstack_v1_signing_and_attestation() -> Result<(), Box<dyn std::error::Error>> {
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
        .get_key("operator/v2", "secp256k1")
        .await?;
    assert_eq!(
        alloy_signer_local::PrivateKeySigner::from_slice(&raw.key)?.address(),
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

    let evidence = DstackAttestor::with_endpoint(endpoint)
        .attest(b"integration-test")
        .await?;
    assert_eq!(evidence.settlement_public_key, public_key);
    assert!(!evidence.quote.is_empty());
    assert!(!evidence.info.app_id.is_empty());
    assert_eq!(evidence.info.compose_hash.len(), 32);
    Ok(())
}
