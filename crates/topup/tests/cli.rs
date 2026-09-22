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
    let commands: &[&[&str]] = &[&["run"], &["migrate"], &["attest"], &["restore-check"]];

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
