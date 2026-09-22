//! Dependency-boundary checks for the pure domain crate.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::expect_used,
        clippy::float_arithmetic,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )
)]

use std::error::Error;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    kind: Option<String>,
}

#[test]
fn core_has_no_runtime_dependencies() -> Result<(), Box<dyn Error>> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("core crate must be nested under crates/")?;
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(workspace_root.join("Cargo.toml"))
        .output()?;

    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    let core = metadata
        .packages
        .iter()
        .find(|package| package.name == "topup-core")
        .ok_or("topup-core was not present in cargo metadata")?;

    assert!(
        core.dependencies.iter().all(|dependency| {
            dependency.kind.as_deref() == Some("dev") || dependency.name == "serde"
        }),
        "topup-core runtime dependencies must be serde or explicitly reviewed as pure and I/O-free"
    );

    Ok(())
}
