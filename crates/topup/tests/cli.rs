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
fn restore_check_requires_a_database_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .arg("restore-check")
        .args([
            "--expected-heartbeat-at",
            "2026-09-22T00:00:00Z",
            "--expected-lsn",
            "0/0",
        ])
        .env_remove("RESTORE_DATABASE_URL")
        .env_remove("MIGRATE_DATABASE_URL")
        .env_remove("DATABASE_URL")
        .output()
        .expect("topup process should start");
    assert!(!output.status.success());
    let output_text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_text.contains("required for restore-check"));
}

#[test]
fn heartbeat_requires_a_database_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .arg("heartbeat")
        .env_remove("DATABASE_URL")
        .output()
        .expect("topup process should start");
    assert!(!output.status.success());
    let output_text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_text.contains("required for heartbeat"));
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
    assert_eq!(value["app_id"], "");
    assert_eq!(value["compose_hash"], "");
}

#[cfg(feature = "dev-signer")]
#[test]
fn development_backup_key_is_written_without_printing_it() {
    let directory = std::env::temp_dir().join(format!("topup-cli-backup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).expect("temporary directory should be created");
    let path = directory.join("backup.key");
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .args(["backup-key", "--dev", "--output"])
        .arg(&path)
        .output()
        .expect("topup process should start");

    assert!(output.status.success());
    let key = std::fs::read_to_string(&path).expect("backup key should be written");
    let fallback = std::fs::read_to_string(directory.join("backup-v0.key"))
        .expect("fallback backup key should be written");
    assert_eq!(key.len(), 64);
    assert_eq!(fallback.len(), 64);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&key));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&key));
    std::fs::remove_dir_all(directory).expect("temporary directory should be removed");
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
