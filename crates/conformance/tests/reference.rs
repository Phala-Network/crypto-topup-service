//! In-process proof that conforming and deliberately broken endpoints are distinguished.

use std::sync::Arc;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use topup_conformance::DEV_SETTLEMENT_SEED;
use topup_conformance::reference::EvidencePolicy;
use topup_conformance::reference::{BrokenVariant, ReferenceConfig, ReferenceState, router};
use topup_conformance::report::TestStatus;
use topup_conformance::signer_handle;
use topup_conformance::suite::{SuiteConfig, SyntheticEvidence};

async fn run_variant(broken: BrokenVariant) -> Result<topup_conformance::report::Report> {
    let state = ReferenceState::new(ReferenceConfig {
        verifying_key: SigningKey::from_bytes(&DEV_SETTLEMENT_SEED).verifying_key(),
        keyid: "settlement/v1".to_owned(),
        per_deposit_cap: 10_000,
        refused_account_id: "conformance-refused".to_owned(),
        processing_account_id: "conformance-processing".to_owned(),
        broken,
        evidence_policy: EvidencePolicy::Synthetic,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router(state)).await;
    });
    let report = topup_conformance::suite::run(SuiteConfig {
        settlement_url: format!("http://{address}/settlements"),
        signer: signer_handle(DEV_SETTLEMENT_SEED)?,
        keyid: "settlement/v1".to_owned(),
        per_deposit_cap: 10_000,
        accepted_account_id: "conformance-accepted".to_owned(),
        refused_account_id: "conformance-refused".to_owned(),
        processing_account_id: "conformance-processing".to_owned(),
        evidence: Arc::new(SyntheticEvidence::new(31_337)),
        check_chain_evidence: true,
    })
    .await?;
    task.abort();
    let _ = task.await;
    Ok(report)
}

#[tokio::test]
async fn conforming_reference_passes_every_test() -> Result<()> {
    let report = run_variant(BrokenVariant::None).await?;
    assert!(report.passed, "{:#?}", report.tests);
    assert!(
        report
            .tests
            .iter()
            .all(|test| test.status == TestStatus::Pass)
    );
    Ok(())
}

#[tokio::test]
async fn each_broken_obligation_is_detected() -> Result<()> {
    for (variant, expected_failure) in [
        (BrokenVariant::Signature, "authentication"),
        (BrokenVariant::Idempotency, "replay"),
        (BrokenVariant::Concurrency, "concurrency"),
        (BrokenVariant::Caps, "per_deposit_cap"),
        (BrokenVariant::Evidence, "chain_evidence"),
        (BrokenVariant::DepositIdentity, "deposit_identity"),
    ] {
        let report = run_variant(variant).await?;
        assert!(
            !report.passed,
            "broken variant {variant:?} unexpectedly passed"
        );
        assert!(
            report
                .tests
                .iter()
                .any(|test| test.id == expected_failure && test.status == TestStatus::Fail),
            "broken variant {variant:?} did not fail {expected_failure}: {:#?}",
            report.tests
        );
    }
    Ok(())
}
