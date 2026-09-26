//! The attested route set, validated once at startup and shared by every consumer.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use alloy_primitives::Address;
use topup_adapters::attestation::OperatorKey;
use topup_adapters::chain::evm::EvmClient;
use topup_core::route::{ChainConfig, DestinationConfig, RouteFile, product_destination};

use crate::rpc_provider::{configured_provider_url, provider_label};

/// Every loaded route version with one shared RPC client per chain provider.
///
/// Construction checks everything that must agree across routes: unique versions, one finality
/// rule and provider list per chain, one route name per chain asset, one settlement destination
/// per product, and one destination unit across rate-lock routes.
#[derive(Debug, Default)]
pub struct RouteSet {
    routes: Vec<RouteFile>,
    current: BTreeMap<(u64, Address), usize>,
    chains: BTreeMap<u64, ChainEntry>,
}

#[derive(Debug)]
struct ChainEntry {
    /// Chain settings of the first loaded route; every other route on the chain agrees.
    config: ChainConfig,
    /// One client per `rpc_providers` entry, or why the entry is unusable.
    providers: Vec<Result<Arc<EvmClient>, ProviderError>>,
}

/// A provider entry that cannot be used.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProviderError {
    /// The provider id's environment variable is unset or empty.
    #[error("{environment} is required for `{label}`")]
    MissingUrl {
        /// Log-safe provider label.
        label: String,
        /// Environment variable that must hold the URL.
        environment: String,
    },
    /// The resolved value is not a URL.
    #[error("provider `{label}` has an invalid URL")]
    InvalidUrl {
        /// Log-safe provider label.
        label: String,
    },
    /// No loaded route configures this chain or provider position.
    #[error("chain {chain_id} has no RPC provider {index}")]
    Unconfigured {
        /// EVM chain identifier.
        chain_id: u64,
        /// Position in `chain.rpc_providers`.
        index: usize,
    },
}

impl RouteSet {
    /// Validates the loaded routes and creates one client per chain provider.
    ///
    /// A provider whose URL is missing or invalid does not fail construction; the consumer that
    /// needs it fails instead, so commands that use only provider A do not require provider B.
    pub fn new(routes: Vec<RouteFile>) -> Result<Self, String> {
        let mut versions = BTreeSet::new();
        for route in &routes {
            if !versions.insert((route.route.as_str(), route.version)) {
                return Err(format!(
                    "duplicate route `{}` version {}",
                    route.route, route.version
                ));
            }
        }
        // Settlement calls and product authentication read the destination from the routes, so
        // every route of one product must name the same settlement URL and product key id.
        let products = routes
            .iter()
            .map(|route| route.destination.product.as_str())
            .collect::<BTreeSet<_>>();
        for product in products {
            product_destination(&routes, product).map_err(|error| error.to_string())?;
        }
        // Rate-lock exposure caps sum credit across routes, so every quote-first route must
        // count credit in the same destination minor unit.
        let mut lock_routes = routes.iter().filter(|route| route.rate_lock.enabled);
        if let Some(first) = lock_routes.next()
            && let Some(other) = lock_routes
                .find(|route| route.destination.unit_decimals != first.destination.unit_decimals)
        {
            return Err(format!(
                "rate-lock routes `{}` and `{}` use different destination.unit_decimals; exposure caps require one unit",
                first.route, other.route
            ));
        }

        let mut current = BTreeMap::<(u64, Address), usize>::new();
        let mut chains = BTreeMap::<u64, ChainEntry>::new();
        for (index, route) in routes.iter().enumerate() {
            route.validate().map_err(|error| {
                format!(
                    "route `{}` version {} failed validation: {error}",
                    route.route, route.version
                )
            })?;
            if route.chain.finality != "finalized" {
                return Err(format!(
                    "route `{}` version {} uses unsupported finality rule `{}`",
                    route.route, route.version, route.chain.finality
                ));
            }
            let chain_id = route.chain.chain_id;
            let chain = chains.entry(chain_id).or_insert_with(|| ChainEntry {
                config: route.chain.clone(),
                providers: route
                    .chain
                    .rpc_providers
                    .iter()
                    .enumerate()
                    .map(|(position, provider)| resolve_provider(provider, position))
                    .collect(),
            });
            let first = &chain.config;
            if first.finality != route.chain.finality
                || first.rpc_providers != route.chain.rpc_providers
            {
                return Err(format!(
                    "route `{}` version {} disagrees with another chain {chain_id} scanner configuration",
                    route.route, route.version
                ));
            }
            let asset = (chain_id, route.asset.contract);
            match current
                .get(&asset)
                .and_then(|selected| routes.get(*selected))
            {
                Some(selected) if selected.route != route.route => {
                    return Err(format!(
                        "routes `{}` and `{}` both select chain {chain_id} asset {:#x}",
                        selected.route, route.route, route.asset.contract
                    ));
                }
                Some(selected) if selected.version >= route.version => {}
                Some(_) | None => {
                    current.insert(asset, index);
                }
            }
        }
        Ok(Self {
            routes,
            current,
            chains,
        })
    }

    /// Returns every loaded route version, in load order.
    #[must_use]
    pub fn routes(&self) -> &[RouteFile] {
        &self.routes
    }

    /// Returns the highest loaded version of each chain asset's route, ordered by chain and asset.
    pub fn current(&self) -> impl Iterator<Item = &RouteFile> {
        self.current
            .values()
            .filter_map(|index| self.routes.get(*index))
    }

    /// Returns the operator key each chain's flusher signs with, in ascending chain order.
    ///
    /// The version comes from the chain's current routes, which must all name the same
    /// `operator_key_version`.
    pub fn operator_keys(&self) -> Result<Vec<OperatorKey>, String> {
        let mut keys = BTreeMap::<u64, (&str, OperatorKey)>::new();
        for route in self.current() {
            let chain_id = route.chain.chain_id;
            let key = OperatorKey {
                chain_id,
                key_version: route
                    .chain
                    .operator_key_version()
                    .map_err(|error| error.to_string())?,
            };
            if let Some((other, other_key)) = keys.insert(chain_id, (&route.route, key))
                && other_key != key
            {
                return Err(format!(
                    "current routes `{other}` and `{}` on chain {chain_id} use operator key \
                     versions {} and {}; they must share one operator_key_version",
                    route.route, other_key.key_version, key.key_version
                ));
            }
        }
        Ok(keys.into_values().map(|(_, key)| key).collect())
    }

    /// Returns the configured chain ids in ascending order.
    pub fn chain_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.chains.keys().copied()
    }

    /// Returns the shared settings of a configured chain.
    #[must_use]
    pub fn chain(&self, chain_id: u64) -> Option<&ChainConfig> {
        self.chains.get(&chain_id).map(|chain| &chain.config)
    }

    /// Returns the shared client of the provider at `index` in the chain's `rpc_providers`.
    pub fn provider(&self, chain_id: u64, index: usize) -> Result<&Arc<EvmClient>, ProviderError> {
        self.chains
            .get(&chain_id)
            .and_then(|chain| chain.providers.get(index))
            .ok_or(ProviderError::Unconfigured { chain_id, index })?
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Returns the attested destination of `product`, or `None` when no route names it.
    #[must_use]
    pub fn destination(&self, product: &str) -> Option<&DestinationConfig> {
        self.routes
            .iter()
            .find(|route| route.destination.product == product)
            .map(|route| &route.destination)
    }
}

fn resolve_provider(provider: &str, index: usize) -> Result<Arc<EvmClient>, ProviderError> {
    let label = provider_label(provider, index);
    match configured_provider_url(provider) {
        Ok(url) => EvmClient::new(&url)
            .map(|client| Arc::new(client.with_provider(label.clone())))
            .map_err(|_| ProviderError::InvalidUrl { label }),
        Err(environment) => Err(ProviderError::MissingUrl { label, environment }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> RouteFile {
        serde_saphyr::from_str(include_str!("../tests/fixtures/phala-cloud-pha.yaml"))
            .expect("route fixture parses")
    }

    #[test]
    fn route_loading_accepts_versions_and_rejects_exact_duplicates() {
        let route = fixture();
        let mut newer = route.clone();
        newer.version = route.version + 1;

        let set = RouteSet::new(vec![route.clone(), newer]).expect("versions load");
        assert_eq!(
            set.current().map(|route| route.version).collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(
            RouteSet::new(vec![route.clone(), route]).map(|_| ()),
            Err("duplicate route `phala-cloud-ethereum-pha-usd` version 1".to_owned())
        );
    }

    #[test]
    fn route_loading_requires_one_settlement_destination_per_product() {
        let route = fixture();
        let mut newer = route.clone();
        newer.version = route.version + 1;
        newer.destination.settlement_url = "https://other.example/settlements".to_owned();
        assert!(
            RouteSet::new(vec![route.clone(), newer.clone()])
                .expect_err("one product must not have two settlement URLs")
                .contains("destination.settlement_url")
        );

        newer.destination.settlement_url = route.destination.settlement_url.clone();
        newer.destination.product_kid = "phala-cloud/v2".to_owned();
        assert!(
            RouteSet::new(vec![route.clone(), newer.clone()])
                .expect_err("one product must not have two key ids")
                .contains("destination.product_kid")
        );

        newer.route = "builder-route".to_owned();
        newer.asset.contract = Address::repeat_byte(0x42);
        newer.destination.product = "builder".to_owned();
        newer.destination.settlement_url = "https://builder.example/settlements".to_owned();
        let set = RouteSet::new(vec![route.clone(), newer]).expect("two products load");
        assert_eq!(
            set.destination("builder")
                .map(|destination| destination.settlement_url.as_str()),
            Some("https://builder.example/settlements")
        );
        assert_eq!(set.destination("unknown"), None);
    }

    #[test]
    fn route_loading_requires_one_unit_for_rate_lock_exposure() {
        let route = fixture();
        let mut other = route.clone();
        other.route = "other-route".to_owned();
        other.asset.contract = Address::repeat_byte(0x42);
        other.destination.unit_decimals = route.destination.unit_decimals + 1;

        assert_eq!(
            RouteSet::new(vec![route.clone(), other.clone()]).map(|_| ()),
            Err(format!(
                "rate-lock routes `{}` and `other-route` use different destination.unit_decimals; exposure caps require one unit",
                route.route
            ))
        );
        other.rate_lock.enabled = false;
        RouteSet::new(vec![route, other]).expect("one rate-lock unit loads");
    }

    #[test]
    fn one_chain_has_one_finality_rule_provider_list_and_route_per_asset() {
        let route = fixture();
        let mut finality = route.clone();
        finality.chain.finality = "latest".to_owned();
        assert!(
            RouteSet::new(vec![finality])
                .expect_err("only finalized is reviewed")
                .contains("unsupported finality rule")
        );

        let mut providers = route.clone();
        providers.route = "other-route".to_owned();
        providers.asset.contract = Address::repeat_byte(0x42);
        providers.chain.rpc_providers = vec!["a".to_owned(), "b".to_owned()];
        assert!(
            RouteSet::new(vec![route.clone(), providers])
                .expect_err("one chain has one provider list")
                .contains("disagrees with another chain 1")
        );

        let mut same_asset = route.clone();
        same_asset.route = "other-route".to_owned();
        assert!(
            RouteSet::new(vec![route, same_asset])
                .expect_err("one asset has one route name")
                .contains("both select chain 1 asset")
        );
    }

    #[test]
    fn providers_resolve_lazily_with_log_safe_labels() {
        let mut route = fixture();
        route.chain.rpc_providers = vec![
            "https://user:secret@rpc.example/v1?key=secret".to_owned(),
            "r1-routeset-unset-provider".to_owned(),
        ];
        let set = RouteSet::new(vec![route]).expect("missing provider B does not fail loading");

        let primary = set.provider(1, 0).expect("inline URL resolves");
        assert_eq!(
            primary.endpoint().to_string(),
            "provider `rpc_providers[0]`"
        );
        assert_eq!(
            set.provider(1, 1).map(|_| ()),
            Err(ProviderError::MissingUrl {
                label: "r1-routeset-unset-provider".to_owned(),
                environment: "TOPUP_RPC_R1_ROUTESET_UNSET_PROVIDER_URL".to_owned(),
            })
        );
        assert_eq!(
            set.provider(2, 0).map(|_| ()),
            Err(ProviderError::Unconfigured {
                chain_id: 2,
                index: 0
            })
        );
    }
}
