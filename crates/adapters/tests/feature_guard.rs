//! Compile-time check for the release development-signer guard.

use std::path::Path;
use std::process::Command;

#[test]
fn release_builds_reject_the_dev_signer_feature() -> Result<(), Box<dyn std::error::Error>> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dev_feature_guard.rs");
    let output = Command::new("rustc")
        .args([
            "--crate-type=lib",
            "--edition=2024",
            "--cfg",
            "feature=\"dev-signer\"",
            "-C",
            "debug-assertions=off",
        ])
        .arg(source)
        .output()?;

    assert!(
        !output.status.success(),
        "release guard must fail compilation"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("dev-signer feature must not be enabled in release builds")
    );
    Ok(())
}
