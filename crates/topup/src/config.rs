//! The service configuration file (`--config`, docs/configuration.md): the public settings of one
//! deployment, attested with the compose that inlines it.
//!
//! [`Config::parse`] is the only validation of the file. It reads no secret, so `topup config
//! check` and `config show` run where no key exists (CI, a first provisioning, Deploy's unsealed
//! preflight), and a keyed provider's URL keeps its `{key}`. The keys are resolved only by
//! [`Config::route_set`] (the service) and [`Config::check_secrets`] (a preflight that holds them),
//! both through [`ProviderUrl::resolve`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use topup_core::route::RouteFile;

use crate::api::{PublicOrigin, VerificationKey};
use crate::routes::RouteSet;
use crate::rpc_provider::{ProviderUrl, environment_key, key_environment};

/// The file as written. Every field is required; an unknown field is an error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigSpec {
    environment: String,
    public_origin: String,
    admin_key: AdminKeySpec,
    rpc_providers: BTreeMap<String, String>,
    routes: Vec<RouteFile>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AdminKeySpec {
    id: String,
    public_key: String,
}

/// A validated service configuration.
#[derive(Clone, Debug)]
pub struct Config {
    /// The deployment's name, reported to Sentry (`<environment>-restore` while read-only). A tag
    /// only: no deployment policy reads it.
    pub environment: String,
    /// The API's one public origin: admin signatures and treasury challenges name it.
    pub public_origin: PublicOrigin,
    /// The operator's admin verification key.
    pub admin_key: VerificationKey,
    /// Each provider id's URL; a keyed one keeps `{key}` here.
    pub rpc_providers: BTreeMap<String, ProviderUrl>,
    /// Every enabled route version.
    pub routes: Vec<RouteFile>,
    admin_key_spec: AdminKeySpec,
}

impl std::fmt::Debug for AdminKeySpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdminKeySpec")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The resolved configuration as `topup config show` prints it: every route default written out.
#[derive(Serialize)]
struct ResolvedConfig<'a> {
    environment: &'a str,
    public_origin: String,
    admin_key: &'a AdminKeySpec,
    rpc_providers: &'a BTreeMap<String, ProviderUrl>,
    routes: &'a [RouteFile],
}

impl Config {
    /// Reads and validates a configuration file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let yaml = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?;
        Self::parse(&yaml)
            .map_err(|error| format!("invalid configuration `{}`: {error}", path.display()))
    }

    /// Parses and validates a configuration, with no secret: see the module documentation.
    pub fn parse(yaml: &str) -> Result<Self, String> {
        let spec: ConfigSpec =
            serde_saphyr::from_str(yaml).map_err(|error| format!("invalid YAML: {error}"))?;
        if !is_name(&spec.environment, 64) {
            return Err(
                "environment must be 1-64 lowercase letters, digits, `-`, or `_`, starting with a \
                 letter or digit"
                    .to_owned(),
            );
        }
        let public_origin = PublicOrigin::parse(&spec.public_origin)
            .map_err(|error| format!("public_origin: {error}"))?;
        let admin_key =
            VerificationKey::from_base64(spec.admin_key.id.clone(), &spec.admin_key.public_key)
                .map_err(|error| format!("admin_key.public_key: {error}"))?;
        if spec.admin_key.id.is_empty() || spec.admin_key.id.chars().any(char::is_whitespace) {
            return Err("admin_key.id must be a non-empty key id without spaces".to_owned());
        }
        let mut rpc_providers = BTreeMap::new();
        for (id, url) in &spec.rpc_providers {
            if !is_name(id, 64) || id.contains('_') {
                return Err(format!(
                    "rpc_providers: `{id}` is not a provider id (lowercase letters, digits, `-`)"
                ));
            }
            let url = ProviderUrl::parse(url)
                .map_err(|problem| format!("rpc_providers.{id}: the URL {problem}"))?;
            rpc_providers.insert(id.clone(), url);
        }
        if spec.routes.is_empty() {
            return Err("routes must list at least one route".to_owned());
        }
        RouteSet::check(&spec.routes)?;
        check_providers(&spec.routes, &rpc_providers)?;
        Ok(Self {
            environment: spec.environment,
            public_origin,
            admin_key,
            rpc_providers,
            routes: spec.routes,
            admin_key_spec: spec.admin_key,
        })
    }

    /// The routes with each provider's client, its key read from `TOPUP_RPC_<ID>_KEY`.
    pub fn route_set(&self) -> Result<RouteSet, String> {
        RouteSet::with_providers(self.routes.clone(), &self.rpc_providers)
    }

    /// Checks that every provider's sealed key fits its URL: present for a `{key}`, absent
    /// otherwise, and URL-safe. Errors name the variable, never a value.
    pub fn check_secrets(&self, key: impl Fn(&str) -> Option<String>) -> Result<(), String> {
        let problems = self
            .rpc_providers
            .iter()
            .filter_map(|(id, url)| {
                url.resolve(key(id).as_deref())
                    .err()
                    .map(|problem| format!("{} {problem} (provider {id})", key_environment(id)))
            })
            .collect::<Vec<_>>();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    /// [`Config::check_secrets`] with the keys of the process environment.
    pub fn check_environment_secrets(&self) -> Result<(), String> {
        self.check_secrets(environment_key)
    }

    /// The resolved configuration as pretty JSON, which is also a valid configuration file.
    pub fn resolved_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&ResolvedConfig {
            environment: &self.environment,
            public_origin: self.public_origin.to_string(),
            admin_key: &self.admin_key_spec,
            rpc_providers: &self.rpc_providers,
            routes: &self.routes,
        })
        .map(|json| json + "\n")
        .map_err(|error| format!("failed to write the configuration: {error}"))
    }
}

/// Lowercase letters, digits, `-`, and `_`, starting with a letter or digit.
fn is_name(value: &str, max: usize) -> bool {
    value.len() <= max
        && value
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
}

/// Every provider a route names is configured, none is unused, a provider serves one chain, and a
/// chain's providers are different URLs.
fn check_providers(
    routes: &[RouteFile],
    providers: &BTreeMap<String, ProviderUrl>,
) -> Result<(), String> {
    let mut chain_of = BTreeMap::<&str, u64>::new();
    for route in routes {
        let chain_id = route.chain.chain_id;
        let mut urls = BTreeSet::new();
        for id in &route.chain.rpc_providers {
            let url = providers.get(id.as_str()).ok_or_else(|| {
                format!(
                    "route `{}` names the RPC provider `{id}`, which rpc_providers does not \
                     configure",
                    route.route
                )
            })?;
            if !urls.insert(url.as_str()) {
                return Err(format!(
                    "route `{}`: two of its RPC providers have the same URL; a chain's providers \
                     must be different providers",
                    route.route
                ));
            }
            match chain_of.insert(id.as_str(), chain_id) {
                Some(other) if other != chain_id => {
                    return Err(format!(
                        "RPC provider `{id}` is named on chain {other} and chain {chain_id}; each \
                         chain needs providers of its own"
                    ));
                }
                _ => {}
            }
        }
    }
    if let Some(unused) = providers
        .keys()
        .find(|id| !chain_of.contains_key(id.as_str()))
    {
        return Err(format!(
            "rpc_providers configures `{unused}`, but no route names it"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str = include_str!("../tests/fixtures/phala-cloud-pha.yaml");
    const KEY: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";

    fn config(providers: &str, origin: &str) -> String {
        let route = ROUTE
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "environment: staging\npublic_origin: {origin}\nadmin_key:\n  id: admin/staging-v1\n  \
             public_key: {KEY}\nrpc_providers:\n{providers}\nroutes:\n  -\n{route}\n"
        )
    }

    const PROVIDERS: &str = "  alchemy: https://eth-mainnet.g.alchemy.com/v2/{key}\n  quicknode: https://rpc.example/eth\n";

    #[test]
    fn a_valid_configuration_parses_without_any_secret() {
        let parsed = Config::parse(&config(PROVIDERS, "https://pay.example")).expect("valid");
        assert_eq!(parsed.public_origin.to_string(), "https://pay.example");
        assert_eq!(parsed.admin_key.kid, "admin/staging-v1");
        let shown = parsed.resolved_json().expect("show");
        assert!(shown.contains("https://eth-mainnet.g.alchemy.com/v2/{key}"));
        let reparsed = Config::parse(&shown).expect("show prints a valid configuration");
        assert_eq!(reparsed.rpc_providers, parsed.rpc_providers);
    }

    #[test]
    fn secrets_are_checked_only_on_request_and_by_the_same_rule() {
        let parsed = Config::parse(&config(PROVIDERS, "https://pay.example")).expect("valid");
        let missing = parsed
            .check_secrets(|_| None)
            .expect_err("the key is missing");
        assert!(
            missing.contains("TOPUP_RPC_ALCHEMY_KEY is required"),
            "{missing}"
        );
        assert!(!missing.contains("QUICKNODE"), "{missing}");
        parsed
            .check_secrets(|id| (id == "alchemy").then(|| "0123456789abcdef".to_owned()))
            .expect("the keyed provider has its key");
        let stray = parsed
            .check_secrets(|_| Some("0123456789abcdef".to_owned()))
            .expect_err("a key for a keyless URL");
        assert!(stray.contains("TOPUP_RPC_QUICKNODE_KEY is set"), "{stray}");
    }

    #[test]
    fn invalid_configurations_are_refused_with_the_reason() {
        for (yaml, reason) in [
            (config(PROVIDERS, "https://pay.example/v1"), "public_origin"),
            (
                config(
                    "  alchemy: https://{key}/v2\n  quicknode: https://rpc.example/eth\n",
                    "https://pay.example",
                ),
                "whole path segment",
            ),
            (
                config("  alchemy: https://rpc.example/a\n", "https://pay.example"),
                "`quicknode`",
            ),
            (
                config(
                    &format!("{PROVIDERS}  spare: https://spare.example\n"),
                    "https://pay.example",
                ),
                "no route names it",
            ),
            (
                config(
                    "  alchemy: https://rpc.example/a\n  quicknode: https://rpc.example/a\n",
                    "https://pay.example",
                ),
                "same URL",
            ),
            (
                config(
                    "  Alchemy: https://rpc.example/a\n  quicknode: https://rpc.example/b\n",
                    "https://pay.example",
                ),
                "not a provider id",
            ),
            (
                config(PROVIDERS, "https://pay.example")
                    .replace("environment: staging\n", "environment: staging\nextra: 1\n"),
                "unknown field",
            ),
        ] {
            let error = Config::parse(&yaml).expect_err(reason);
            assert!(error.contains(reason), "{reason}: {error}");
        }
        // Local stacks and rehearsals use http providers; preflight requires https for a CVM.
        Config::parse(&config(
            "  alchemy: http://anvil:8545/?key={key}\n  quicknode: http://anvil:8545\n",
            "https://pay.example",
        ))
        .expect("http providers");
    }

    /// Every committed configuration validates without secrets, as CI and Deploy's unsealed
    /// preflight run it.
    #[test]
    fn every_committed_configuration_validates() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        let mut directories = vec![root.join("deploy/environments")];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(&directory).expect("environments directory") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    directories.push(path);
                } else if path.file_name().is_some_and(|name| name == "topup.yaml") {
                    files.push(path);
                }
            }
        }
        assert!(files.len() >= 2, "{files:?}");
        for file in files {
            Config::load(&file).unwrap_or_else(|error| panic!("{error}"));
        }
    }
}
