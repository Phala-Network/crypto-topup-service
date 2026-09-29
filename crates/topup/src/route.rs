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

/// The resolved route as JSON, which is also a YAML route file: every default written out,
/// parsing back to the same route.
pub(crate) fn resolved_json(route: &RouteFile) -> Result<String, String> {
    serde_json::to_string_pretty(route)
        .map(|json| json + "\n")
        .map_err(|error| format!("failed to write the route: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = include_str!("../tests/fixtures/phala-cloud-pha.yaml");
    const TEMPLATE: &str = include_str!("../../../examples/phala-cloud-pha.yaml");
    const DEPLOY_ROUTE: &str =
        include_str!("../../../deploy/config/routes/phala-cloud-sepolia-pha.yaml");
    const DEPLOY_USDC_ROUTE: &str =
        include_str!("../../../deploy/config/routes/phala-cloud-sepolia-usdc.yaml");

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
    fn staging_route_resolves_to_the_reviewed_values() {
        let route = parse_and_validate(DEPLOY_ROUTE, false).expect("staging route must pass");
        assert!(!route.livemode, "Sepolia is a test route");
        assert_eq!(
            route.chain.confirmations,
            topup_core::route::Confirmations::Depth(2)
        );
        assert_eq!(route.asset.backstop, topup_core::route::Backstop::Token);
        assert_eq!(
            format!("{:#x}", route.chain.contracts.implementation),
            "0x49f2f1f1a25269ea0c6ff2ab1c7b09dcbe9c5ba9"
        );
        assert_eq!(route.chain.rpc_providers, ["provider-a", "provider-b"]);
        let check = route
            .pricing
            .check
            .as_ref()
            .expect("spot route has a check");
        assert_eq!(
            (check.fx.source.as_str(), check.fx.pair.as_str()),
            ("kraken", "USDT/USD")
        );
        assert!(route.screening.min_deposit_atomic.value().is_zero());
        assert_eq!(route.rate_lock.window_s, 900);
        assert_eq!(route.rate_lock.spread_bps.value(), 50);
        assert_eq!(route.alerts.stuck_after_s.confirmed, 1_800);
    }

    #[test]
    fn staging_usdc_route_is_a_stablecoin_route_beside_pha() {
        let pha = parse_and_validate(DEPLOY_ROUTE, false).expect("staging route must pass");
        let usdc = parse_and_validate(DEPLOY_USDC_ROUTE, false).expect("USDC route must pass");
        assert!(!usdc.livemode, "Sepolia is a test route");
        assert_eq!(
            usdc.chain, pha.chain,
            "one chain has one set of chain settings"
        );
        assert_eq!(
            (usdc.asset.symbol.as_str(), usdc.asset.decimals),
            ("usdc", 6)
        );
        assert_eq!(usdc.asset.backstop, topup_core::route::Backstop::Addresses);
        assert_eq!(
            usdc.pricing.mode,
            topup_core::route::PricingMode::Stablecoin
        );
        assert_eq!(
            (
                usdc.pricing.primary.source.as_str(),
                usdc.pricing.primary.asset.as_str()
            ),
            ("coinmetrics", "usdc")
        );
        assert_eq!(usdc.pricing.check, None);

        // The service loads both, and the USDC route puts the whole chain in address mode.
        let routes = topup::routes::RouteSet::new(vec![pha, usdc]).expect("both routes load");
        assert_eq!(routes.current_in(false).count(), 2);
        let chains = topup::scanner::chain_routes(&routes);
        assert_eq!(chains.len(), 1);
        assert!(!chains[0].token_mode());
    }

    #[test]
    fn resolved_json_parses_back_to_the_same_route() {
        for yaml in [VALID, DEPLOY_ROUTE, DEPLOY_USDC_ROUTE] {
            let route = parse_and_validate(yaml, false).expect("route must pass");
            let resolved = resolved_json(&route).expect("route serializes");
            assert!(resolved.contains("\"implementation\"") && resolved.contains("\"window_s\""));
            assert_eq!(parse_and_validate(&resolved, false), Ok(route));
        }
    }

    #[test]
    fn chain_defaults_are_required_where_the_chain_has_none() {
        let without_chain_overrides = VALID.replace(
            "  sanctions_oracle: \"0x40C57923924B5c5c5455c48D93317139ADDaC8fb\"\n",
            "",
        );
        let mainnet =
            parse_and_validate(&without_chain_overrides, false).expect("chain 1 defaults");
        assert_eq!(
            mainnet.screening.sanctions_oracle,
            topup_core::route::default_sanctions_oracle(1).expect("mainnet oracle")
        );
        let sepolia = without_chain_overrides.replace("  chain_id: 1\n", "  chain_id: 11155111\n");
        assert!(
            parse_and_validate(&sepolia, false)
                .expect_err("no Chainalysis oracle on Sepolia")
                .contains("chain.sanctions_oracle")
        );
        let not_usdt = VALID.replace("symbol: PHAUSDT", "symbol: PHABTC");
        assert!(
            parse_and_validate(&not_usdt, false)
                .expect_err("a non-USDT market needs its FX leg")
                .contains("pricing.check.fx")
        );
    }

    #[test]
    fn route_files_with_removed_keys_fail_with_the_key_name() {
        for (yaml, key) in [
            (
                VALID.replace("  chain_id: 1\n", "  chain_id: 1\n  finality: finalized\n"),
                "finality",
            ),
            // Routes belong to no product: any account quotes on the routes of its mode.
            (
                VALID.replace("livemode: true\n", "livemode: true\nproduct: phala-cloud\n"),
                "product",
            ),
            (
                VALID.replace(
                    "  min_credit_minor: 100\n",
                    "  min_credit_minor: 100\n  enabled: true\n",
                ),
                "enabled",
            ),
            // The service sends no transactions: no operator key, flush schedule, or gas policy.
            (
                VALID.replace(
                    "  rpc_providers: [alchemy, quicknode]\n",
                    "  rpc_providers: [alchemy, quicknode]\n  operator_key_version: 1\n",
                ),
                "operator_key_version",
            ),
            (
                VALID.replace(
                    "  rpc_providers: [alchemy, quicknode]\n",
                    "  rpc_providers: [alchemy, quicknode]\n  flush:\n    schedule: \"0 * * * *\"\n",
                ),
                "flush",
            ),
            (
                VALID.replace(
                    "  min_credit_minor: 100\n",
                    "  min_credit_minor: 100\n  min_flush_atomic: \"1\"\n",
                ),
                "min_flush_atomic",
            ),
            (
                format!("{VALID}alerts:\n  stuck_after_s:\n    credited: 60\n"),
                "credited",
            ),
        ] {
            assert_ne!(yaml, VALID, "fixture edit for `{key}` must apply");
            let error =
                parse_and_validate(&yaml, false).expect_err("removed keys must be rejected");
            assert!(
                error.contains("unknown field") && error.contains(&format!("`{key}`")),
                "unclear error for `{key}`: {error}"
            );
        }
    }

    #[test]
    fn invalid_route_boundaries_fail_with_field_context() {
        for (yaml, field) in [
            (
                VALID.replace("decimals: 18", "decimals: 37"),
                "asset.decimals",
            ),
            (
                format!("{VALID}quote:\n  window_s: 0\n"),
                "quote.window_s",
            ),
            (
                format!("{VALID}quote:\n  spread_bps: 10001\n"),
                "spread_bps",
            ),
            (
                VALID.replace("symbol: pha\n", "symbol: PHA\n"),
                "asset.symbol",
            ),
            // Chain 1 is a mainnet, so its route is live.
            (
                VALID.replace("livemode: true\n", "livemode: false\n"),
                "livemode",
            ),
            (VALID.replace("livemode: true\n", ""), "livemode"),
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
                    "  rpc_providers: [alchemy, quicknode]\n",
                    "  rpc_providers: [alchemy, quicknode]\n  implementation: \"0x0000000000000000000000000000000000000000\"\n",
                ),
                "chain.implementation",
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

    #[test]
    fn pricing_mode_defaults_to_spot_and_stablecoin_may_omit_check() {
        let route = parse_and_validate(VALID, false).expect("valid fixture");
        assert_eq!(route.pricing.mode, topup_core::route::PricingMode::Spot);
        assert_eq!(
            route.pricing.max_fx_deviation_bps.map(|bps| bps.value()),
            Some(50)
        );

        let stablecoin = VALID.replacen(
            "  check: { source: binance, symbol: PHAUSDT }\n",
            "  mode: stablecoin\n",
            1,
        );
        let route = parse_and_validate(&stablecoin, false).expect("stablecoin check is optional");
        assert_eq!(route.pricing.max_fx_deviation_bps, None);

        let spot_without_check =
            VALID.replacen("  check: { source: binance, symbol: PHAUSDT }\n", "", 1);
        assert!(
            parse_and_validate(&spot_without_check, false)
                .expect_err("spot check must be required")
                .contains("pricing.check")
        );
    }
}
