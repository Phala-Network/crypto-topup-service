//! Syntax checks for committed observability deployment artifacts.

use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

#[test]
fn prometheus_rules_are_valid_yaml_with_expected_alerts() -> Result<()> {
    let rules: Value =
        serde_saphyr::from_str(include_str!("../../../deploy/alerts/prometheus-rules.yml"))
            .context("parse Prometheus rules YAML")?;
    let groups = rules["groups"].as_array().context("groups array")?;
    ensure!(!groups.is_empty(), "at least one rule group is required");
    let alerts = groups
        .iter()
        .flat_map(|group| group["rules"].as_array().into_iter().flatten())
        .filter_map(|rule| rule["alert"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "TopupDepositStateAgeExceeded",
        "TopupReconciliationMismatch",
        "TopupScannerLag",
        "TopupBackupTooOld",
        "TopupLoopStopped",
        "TopupOperatorGasReserveLow",
        "TopupLockExposureNearCap",
        "TopupUnsupportedInflows",
    ] {
        ensure!(alerts.contains(&expected), "missing alert {expected}");
    }
    Ok(())
}

#[test]
fn grafana_dashboard_is_valid_json() -> Result<()> {
    let dashboard: Value = serde_json::from_str(include_str!(
        "../../../deploy/dashboards/crypto-topup-service.json"
    ))
    .context("parse Grafana dashboard JSON")?;
    ensure!(
        dashboard["panels"]
            .as_array()
            .is_some_and(|panels| panels.len() >= 11)
    );
    Ok(())
}
