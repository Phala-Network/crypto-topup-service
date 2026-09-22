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
    let commands: &[&[&str]] = &[
        &["run"],
        &["migrate"],
        &["route", "validate", "route.toml"],
        &["restore-check"],
    ];

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
fn attest_requires_a_hex_nonce() {
    let missing = topup(&["attest"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--nonce"));

    let invalid = topup(&["attest", "--nonce", "not-hex"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("valid hexadecimal"));
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
    let output = topup(&["attest", "--nonce", "00", "--dev"]);
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("attestation should be JSON");
    let object = value.as_object().expect("attestation should be an object");

    assert_eq!(object.len(), 4);
    assert_eq!(value["keyid"], "settlement/v1");
    assert_eq!(value["settlement_pubkey"].as_str().map(str::len), Some(64));
    assert_eq!(value["report_data"].as_str().map(str::len), Some(64));
    assert_eq!(value["quote"], "");
}
