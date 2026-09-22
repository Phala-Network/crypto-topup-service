//! Opt-in dstack v1 simulator coverage.

use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use topup_adapters::attestation::DstackAttestor;
use topup_adapters::signer::dstack::DstackSigner;
use topup_core::Signer as _;

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

    let evidence = DstackAttestor::with_endpoint(endpoint)
        .attest(b"integration-test")
        .await?;
    assert_eq!(evidence.settlement_public_key, public_key);
    assert!(!evidence.quote.is_empty());
    assert!(!evidence.info.app_id.is_empty());
    assert_eq!(evidence.info.compose_hash.len(), 32);
    Ok(())
}
