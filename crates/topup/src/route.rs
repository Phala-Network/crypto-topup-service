//! YAML parsing at the command-line I/O boundary.

use topup_core::route::RouteFile;

pub(crate) fn parse_and_validate(yaml: &str, template: bool) -> Result<RouteFile, String> {
    let route: RouteFile =
        serde_saphyr::from_str(yaml).map_err(|error| format!("invalid route YAML: {error}"))?;
    let validation = if template {
        route.validate_template()
    } else {
        route.validate()
    };
    validation.map_err(|error| error.to_string())?;
    Ok(route)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = include_str!("../tests/fixtures/phala-cloud-pha.yaml");
    const TEMPLATE: &str = include_str!("../../../examples/phala-cloud-pha.yaml");

    #[test]
    fn valid_fixture_parses_and_validates() {
        parse_and_validate(VALID, false).expect("valid fixture must pass");
    }

    #[test]
    fn deployment_template_only_passes_in_template_mode() {
        assert!(
            parse_and_validate(TEMPLATE, false)
                .expect_err("zero placeholders must fail normal validation")
                .contains("forwarder_factory")
        );
        parse_and_validate(TEMPLATE, true).expect("template placeholders must be allowed");
    }

    #[test]
    fn invalid_route_boundaries_fail_with_field_context() {
        for (yaml, field) in [
            (
                VALID.replace("decimals: 18", "decimals: 37"),
                "asset.decimals",
            ),
            (
                VALID.replace("window_s: 900", "window_s: 0"),
                "rate_lock.window_s",
            ),
            (
                VALID.replace("spread_bps: 50", "spread_bps: 10001"),
                "spread_bps",
            ),
            (
                VALID.replace(
                    "rpc_providers: [alchemy, quicknode]",
                    "rpc_providers: [alchemy, alchemy]",
                ),
                "chain.rpc_providers",
            ),
            (
                VALID.replace(
                    "rpc_providers: [alchemy, quicknode]",
                    "rpc_providers: [\"\", \"\"]",
                ),
                "chain.rpc_providers",
            ),
            (
                VALID.replace(
                    "rpc_providers: [alchemy, quicknode]",
                    "rpc_providers: [alchemy]",
                ),
                "chain.rpc_providers",
            ),
            (
                VALID.replace(
                    "implementation:    \"0xfeb1871c9897251C74b39DFC74e577888290faE6\"",
                    "implementation:    \"0x0000000000000000000000000000000000000000\"",
                ),
                "chain.contracts.implementation",
            ),
        ] {
            assert!(
                parse_and_validate(&yaml, false)
                    .expect_err("invalid route must fail")
                    .contains(field),
                "expected error for {field}"
            );
        }
    }
}
