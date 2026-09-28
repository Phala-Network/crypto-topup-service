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

/// `read-only` serves the API of a restored database; the heartbeat writer stays stopped.
#[test]
fn heartbeat_refuses_to_start_while_read_only() {
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .arg("heartbeat")
        .env("DATABASE_URL", "postgres://topup_service@127.0.0.1:1/topup")
        .env("TOPUP_SERVICE_ENABLED", "read-only")
        .output()
        .expect("topup process should start");
    assert!(!output.status.success());
    let output_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output_text.contains("heartbeat is disabled while TOPUP_SERVICE_ENABLED=read-only"),
        "{output_text}"
    );
}

/// The compose starts restore-check on every boot; it acts only after a restore from backup and
/// publishes even a failed check to TOPUP_RESTORE_REPORT_FILE for the read-only `/healthz`.
#[test]
fn restore_check_runs_only_after_a_restore_and_reports_failures() {
    let route = format!(
        "{}/tests/fixtures/phala-cloud-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let report =
        std::env::temp_dir().join(format!("topup-restore-check-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&report);
    let restore_check = |switch: &str| {
        Command::new(env!("CARGO_BIN_EXE_topup"))
            .args(["restore-check", "--route", &route])
            .env_remove("MIGRATE_DATABASE_URL")
            .env("TOPUP_RESTORE_FROM_BACKUP", switch)
            .env("TOPUP_RESTORE_REPORT_FILE", &report)
            .output()
            .expect("topup process should start")
    };

    let output = restore_check("off");
    assert!(output.status.success());
    assert!(!report.exists());

    let output = restore_check("on");
    assert!(!output.status.success());
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).expect("report written"))
            .expect("report is JSON");
    std::fs::remove_file(&report).expect("report removed");
    assert_eq!(written["status"], "failed");
    assert_eq!(
        written["failures"][0],
        "failed to connect to the restored database"
    );
}

#[test]
fn restore_check_needs_a_heartbeat_anchor_for_an_lsn() {
    let output = topup(&[
        "restore-check",
        "--expected-lsn",
        "0/0",
        "--route",
        "unused.yaml",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--expected-heartbeat-at"));
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

const ACCOUNT: &str = "acct_0123456789abcdef0123456789abcdef";

#[test]
fn attest_requires_a_hex_nonce_and_an_account() {
    let missing = topup(&["attest", "--account", ACCOUNT]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--nonce"));

    let no_account = topup(&["attest", "--nonce", "00"]);
    assert!(!no_account.status.success());
    assert!(String::from_utf8_lossy(&no_account.stderr).contains("--account"));

    let invalid = topup(&["attest", "--account", ACCOUNT, "--nonce", "not-hex"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("valid hexadecimal"));

    let empty = topup(&["attest", "--account", ACCOUNT, "--nonce", ""]);
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("non-empty hexadecimal"));

    let oversized = "ab".repeat(33);
    let oversized = topup(&["attest", "--account", ACCOUNT, "--nonce", &oversized]);
    assert!(!oversized.status.success());
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("at most 32 bytes"));

    for (account, version) in [("cus_1", "1"), (ACCOUNT, "0")] {
        let output = topup(&[
            "attest",
            "--account",
            account,
            "--version",
            version,
            "--nonce",
            "00",
        ]);
        assert!(!output.status.success(), "{account} v{version}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("acct_ id"));
    }
}

#[cfg(not(feature = "dev-signer"))]
#[test]
fn dev_attestation_is_not_available_without_the_feature() {
    let output = topup(&["attest", "--account", ACCOUNT, "--nonce", "00", "--dev"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--dev'"));
}

#[cfg(feature = "dev-signer")]
#[test]
fn dev_attestation_prints_the_required_json_shape() {
    let nonce = "ab".repeat(32);
    let output = topup(&["attest", "--account", ACCOUNT, "--nonce", &nonce, "--dev"]);
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("attestation should be JSON");
    let object = value.as_object().expect("attestation should be an object");

    assert_eq!(object.len(), 8);
    assert_eq!(value["object"], "attestation");
    assert_eq!(value["account"], ACCOUNT);
    assert_eq!(value["livemode"], false);
    assert_eq!(value["webhook_keys"][0]["version"], 1);
    assert_eq!(
        value["webhook_keys"][0]["public_key"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    assert_eq!(value["report_data"].as_str().map(str::len), Some(64));
    assert_eq!(value["quote"], "");
    assert_eq!(value["app_id"], "");
    assert_eq!(value["compose_hash"], "");
}

#[cfg(feature = "dev-signer")]
#[test]
fn dev_attestation_binds_the_nonce_account_mode_and_keys_like_the_api() {
    use topup_adapters::attestation::{AttestedWebhookKey, report_data};

    let output = topup(&[
        "attest",
        "--account",
        ACCOUNT,
        "--live",
        "--version",
        "2",
        "--version",
        "1",
        "--nonce",
        "00010203",
        "--dev",
    ]);
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("attestation should be JSON");
    let keys: Vec<AttestedWebhookKey> = value["webhook_keys"]
        .as_array()
        .expect("webhook keys")
        .iter()
        .map(|key| AttestedWebhookKey {
            version: u32::try_from(key["version"].as_u64().expect("version")).expect("u32"),
            public_key: topup_core::Ed25519PublicKey(
                hex::decode(key["public_key"].as_str().expect("public key"))
                    .expect("hex public key")
                    .try_into()
                    .expect("32-byte public key"),
            ),
        })
        .collect();
    assert_eq!(
        keys.iter().map(|key| key.version).collect::<Vec<_>>(),
        [2, 1]
    );
    assert_ne!(keys[0].public_key, keys[1].public_key);
    assert_eq!(value["livemode"], true);
    assert_eq!(
        value["report_data"],
        hex::encode(report_data(&[0, 1, 2, 3], ACCOUNT, true, &keys).expect("report data"))
    );

    // The operator key is gone with the flusher; its options are no longer accepted.
    let removed = topup(&[
        "attest",
        "--account",
        ACCOUNT,
        "--nonce",
        "00",
        "--dev",
        "--operator-key-version",
        "1",
    ]);
    assert!(!removed.status.success());
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
fn route_show_prints_the_resolved_route_as_json() {
    let route = format!(
        "{}/../../deploy/config/routes/phala-cloud-sepolia-pha.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = topup(&["route", "show", &route]);
    assert!(output.status.success());
    let resolved: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("route show prints JSON");
    assert_eq!(
        resolved["chain"]["implementation"],
        "0x70b714508bfa441449dc09f790ca03baa5170360"
    );
    assert_eq!(resolved["quote"]["window_s"], 900);

    let invalid = topup(&["route", "show", "/definitely/missing/route.yaml"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("failed to read"));
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
fn restore_check_help_requires_a_stopped_service() {
    let output = topup(&["restore-check", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--route"));
    assert!(help.contains("processes are stopped"));
}
