//! End-to-end checks for the command-line scaffold.

use std::process::{Command, Output};

fn topup(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_topup"))
        .args(args)
        .output()
        .expect("topup process should start")
}

#[test]
fn help_and_version_succeed() {
    for args in [["--help"].as_slice(), ["--version"].as_slice()] {
        let output = topup(args);
        assert!(output.status.success(), "{args:?} should exit successfully");
    }
}

#[test]
fn placeholder_commands_fail_with_a_clear_message() {
    let commands: &[&[&str]] = &[&["restore-check"]];

    for args in commands {
        let output = topup(args);
        assert!(!output.status.success(), "{args:?} should fail");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("not implemented") || stderr.contains("not implemented"),
            "{args:?} should report that it is not implemented"
        );
    }
}

#[test]
fn run_requires_a_database_url() {
    let route = format!(
        "{}/tests/fixtures/phala-cloud-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .args(["run", "--route", &route])
        .env_remove("DATABASE_URL")
        .env_remove("TOPUP_ADMIN_KID")
        .env_remove("TOPUP_ADMIN_PUBLIC_KEY")
        .output()
        .expect("topup process should start");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("DATABASE_URL is required for run")
            || stderr.contains("DATABASE_URL is required for run")
    );
}

#[test]
fn attest_requires_a_hex_nonce() {
    let missing = topup(&["attest"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--nonce"));

    let invalid = topup(&["attest", "--nonce", "not-hex"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("valid hexadecimal"));

    let empty = topup(&["attest", "--nonce", ""]);
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("non-empty hexadecimal"));

    let oversized = "ab".repeat(33);
    let oversized = topup(&["attest", "--nonce", &oversized]);
    assert!(!oversized.status.success());
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("at most 32 bytes"));
}

#[cfg(not(feature = "dev-signer"))]
#[test]
fn dev_attestation_is_not_available_without_the_feature() {
    let output = topup(&["attest", "--nonce", "00", "--dev"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--dev'"));
}

#[cfg(feature = "dev-signer")]
#[test]
fn dev_attestation_prints_the_required_json_shape() {
    let nonce = "ab".repeat(32);
    let output = topup(&["attest", "--nonce", &nonce, "--dev"]);
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("attestation should be JSON");
    let object = value.as_object().expect("attestation should be an object");

    assert_eq!(object.len(), 6);
    assert_eq!(value["keyid"], "settlement/v1");
    assert_eq!(value["settlement_pubkey"].as_str().map(str::len), Some(64));
    assert_eq!(value["report_data"].as_str().map(str::len), Some(64));
    assert_eq!(value["quote"], "");
    assert_eq!(value["operator_keyid"], "operator/v1");
    assert_eq!(value["operator_address"].as_str().map(str::len), Some(42));
}

#[cfg(feature = "dev-signer")]
#[test]
fn dev_attestation_reports_the_requested_operator_key_version() {
    let attest = |version: &str| {
        let output = topup(&[
            "attest",
            "--nonce",
            "00",
            "--dev",
            "--operator-key-version",
            version,
        ]);
        assert!(output.status.success(), "version {version} should attest");
        serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .expect("attestation should be JSON")
    };
    let v1 = attest("1");
    let v2 = attest("2");

    assert_eq!(v2["operator_keyid"], "operator/v2");
    assert_ne!(v1["operator_address"], v2["operator_address"]);
    assert_eq!(v1["settlement_pubkey"], v2["settlement_pubkey"]);

    let zero = topup(&[
        "attest",
        "--nonce",
        "00",
        "--dev",
        "--operator-key-version",
        "0",
    ]);
    assert!(!zero.status.success());
}

#[test]
fn migrate_requires_a_database_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .arg("migrate")
        .env_remove("MIGRATE_DATABASE_URL")
        .output()
        .expect("topup process should start");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("MIGRATE_DATABASE_URL is required for migrate")
            || stderr.contains("MIGRATE_DATABASE_URL is required for migrate")
    );
}

#[test]
fn route_validate_accepts_the_valid_fixture() {
    let route = format!(
        "{}/tests/fixtures/phala-cloud-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = topup(&["route", "validate", &route]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("valid at schema level"));
    assert!(stdout.contains("on-chain deployment and Safe control were not checked"));
}

#[test]
fn route_validate_requires_template_mode_for_placeholders() {
    let template = format!(
        "{}/../../examples/phala-cloud-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let normal_output = topup(&["route", "validate", &template]);
    assert!(!normal_output.status.success());
    assert!(String::from_utf8_lossy(&normal_output.stderr).contains("forwarder_factory"));

    let template_output = topup(&["route", "validate", "--template", &template]);
    assert!(template_output.status.success());
    assert!(String::from_utf8_lossy(&template_output.stdout).contains("route template"));
}

#[test]
fn route_validate_rejects_invalid_content_and_missing_files() {
    let invalid = format!(
        "{}/../../contracts/test-vectors/create2.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let invalid_output = topup(&["route", "validate", &invalid]);
    assert!(!invalid_output.status.success());
    assert!(String::from_utf8_lossy(&invalid_output.stderr).contains("is invalid"));

    let missing_output = topup(&["route", "validate", "/definitely/missing/route.yaml"]);
    assert!(!missing_output.status.success());
    assert!(String::from_utf8_lossy(&missing_output.stderr).contains("failed to read"));
}

#[test]
fn outbox_replay_validates_selector_and_timestamp_before_connecting() {
    let help = topup(&["outbox", "replay", "--help"]);
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("--id"));
    assert!(help_text.contains("--since"));
    assert!(help_text.contains("--force"));

    let invalid = Command::new(env!("CARGO_BIN_EXE_topup"))
        .args(["outbox", "replay", "--since", "not-a-timestamp"])
        .env_remove("DATABASE_URL")
        .output()
        .expect("topup process should start");
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("RFC 3339")
            || String::from_utf8_lossy(&invalid.stdout).contains("RFC 3339")
    );
}

#[test]
fn reconcile_help_exposes_once_and_post_restore_modes() {
    let output = topup(&["reconcile", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--once"));
    assert!(help.contains("--post-restore"));
    assert!(help.contains("--route"));
}
