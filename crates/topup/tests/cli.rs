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
fn restore_check_requires_owner_credentials() {
    let route = format!(
        "{}/tests/fixtures/phala-cloud-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .arg("restore-check")
        .args([
            "--expected-heartbeat-at",
            "2026-09-22T00:00:00Z",
            "--expected-lsn",
            "0/0",
            "--route",
            &route,
        ])
        .env_remove("MIGRATE_DATABASE_URL")
        // The service login is never a fallback for the owner-only restore gate.
        .env("DATABASE_URL", "postgres://topup_service@127.0.0.1:1/topup")
        .output()
        .expect("topup process should start");
    assert!(!output.status.success());
    let output_text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_text.contains("MIGRATE_DATABASE_URL is required for restore-check"));
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

/// A replacement CVM boots for a restore with `TOPUP_SERVICE_ENABLED=off`; `run` and `heartbeat`
/// must stop before they touch the network or the database.
#[test]
fn service_commands_refuse_to_start_while_disabled_for_a_restore() {
    let route = format!(
        "{}/tests/fixtures/phala-cloud-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    for (args, command) in [
        (vec!["run", "--route", route.as_str()], "run"),
        (vec!["heartbeat"], "heartbeat"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_topup"))
            .args(&args)
            .env("DATABASE_URL", "postgres://topup_service@127.0.0.1:1/topup")
            .env("TOPUP_SERVICE_ENABLED", "off")
            .output()
            .expect("topup process should start");
        assert!(!output.status.success());
        let output_text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output_text.contains(&format!(
                "{command} is disabled while TOPUP_SERVICE_ENABLED=off"
            )),
            "{output_text}"
        );
    }
}

#[test]
fn run_requires_a_valid_public_origin() {
    let route = format!(
        "{}/tests/fixtures/phala-cloud-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    for (origin, message) in [
        (None, "TOPUP_PUBLIC_ORIGIN is required for run"),
        (
            Some("https://topup.example/v1"),
            "public origin must not include a path, query, or fragment",
        ),
        (
            Some("ftp://topup.example"),
            "public origin scheme must be http or https",
        ),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_topup"));
        command
            .args(["run", "--route", &route])
            .env(
                "DATABASE_URL",
                "postgres://unused:unused@127.0.0.1:1/unused",
            )
            .env("TOPUP_ADMIN_KID", "admin/v1")
            .env(
                "TOPUP_ADMIN_PUBLIC_KEY",
                "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=",
            )
            .env_remove("TOPUP_PUBLIC_ORIGIN");
        if let Some(origin) = origin {
            command.env("TOPUP_PUBLIC_ORIGIN", origin);
        }
        let output = command.output().expect("topup process should start");

        assert!(!output.status.success(), "{origin:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains(message) || stderr.contains(message),
            "{origin:?}: expected {message:?}\n{stdout}\n{stderr}"
        );
    }
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

    assert_eq!(object.len(), 9);
    assert_eq!(value["keyid"], "settlement/v1");
    assert_eq!(value["operators"], serde_json::json!([]));
    assert_eq!(value["settlement_pubkey"].as_str().map(str::len), Some(64));
    assert_eq!(value["report_data"].as_str().map(str::len), Some(64));
    assert_eq!(value["quote"], "");
    assert_eq!(value["app_id"], "");
    assert_eq!(value["compose_hash"], "");
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

#[cfg(feature = "dev-signer")]
#[test]
fn dev_attestation_binds_the_route_operators_like_the_api() {
    use topup_adapters::attestation::{AttestedOperator, report_data};

    let route = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/phala-cloud-pha.yaml"
    );
    let output = topup(&["attest", "--nonce", "00010203", "--dev", "--route", route]);
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("attestation should be JSON");

    // The fixture route runs on chain 1 with operator key version 1, the default preview.
    let address = value["operator_address"]
        .as_str()
        .expect("operator address");
    assert_eq!(
        value["operators"],
        serde_json::json!([{
            "chain_id": 1,
            "operator_key_version": 1,
            "keyid": "operator/v1",
            "address": address,
        }])
    );
    let settlement = topup_core::Ed25519PublicKey(
        hex::decode(value["settlement_pubkey"].as_str().expect("settlement key"))
            .expect("hex settlement key")
            .try_into()
            .expect("32-byte settlement key"),
    );
    let operator = AttestedOperator {
        chain_id: 1,
        key_version: std::num::NonZeroU32::MIN,
        address: address.parse().expect("operator address parses"),
    };
    assert_eq!(
        value["report_data"],
        hex::encode(report_data(&[0, 1, 2, 3], &settlement, &[operator]))
    );

    let invalid = topup(&[
        "attest",
        "--nonce",
        "00",
        "--dev",
        "--route",
        "/nonexistent",
    ]);
    assert!(!invalid.status.success());
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
fn reconcile_help_exposes_the_post_restore_mode() {
    let output = topup(&["reconcile", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--post-restore"));
    assert!(help.contains("--route"));
    assert!(help.contains("processes are stopped"));
}

#[test]
fn restore_check_help_requires_a_stopped_service() {
    let output = topup(&["restore-check", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--route"));
    assert!(help.contains("processes are stopped"));
}
