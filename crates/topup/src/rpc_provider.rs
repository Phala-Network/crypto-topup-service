//! Shared RPC provider-id configuration.
//!
//! Routes name their chain's providers by id (`chain.rpc_providers`); deploy preflight checks that
//! each provider serves the chain of every route naming it. A provider id's URL is an attested
//! setting, `TOPUP_RPC_<ID>_URL`. A provider that authenticates
//! with an API key in the URL (Alchemy, Infura, QuickNode, ...) has the placeholder `{key}` where
//! its documentation puts the key, and the key itself is the owner-sealed secret
//! `TOPUP_RPC_<ID>_KEY`, so the attested URL fixes the endpoint without publishing the key.

/// Where an attested provider URL takes its owner-sealed key.
const KEY_PLACEHOLDER: &str = "{key}";
/// Shortest accepted key: every provider key is longer, and the redaction of node messages
/// scrubs URL path segments only from this length on.
const MIN_KEY_CHARS: usize = 8;

/// Why a provider id's URL cannot be resolved; each names the environment variable to fix.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum UnresolvedProvider {
    /// The URL variable is unset or empty.
    MissingUrl(String),
    /// The key variable does not fit the URL.
    Key {
        /// The key variable.
        environment: String,
        /// Log-safe reason, never the value.
        problem: &'static str,
    },
}

/// Resolves either an inline HTTP URL or a provider id through its environment variables.
pub(crate) fn configured_provider_url(provider: &str) -> Result<String, UnresolvedProvider> {
    if provider.contains("://") {
        return Ok(provider.to_owned());
    }
    let url_environment = provider_environment_name(provider, "URL");
    let url =
        non_empty_env(&url_environment).ok_or(UnresolvedProvider::MissingUrl(url_environment))?;
    let key_environment = provider_environment_name(provider, "KEY");
    with_key(&url, non_empty_env(&key_environment).as_deref()).map_err(|problem| {
        UnresolvedProvider::Key {
            environment: key_environment,
            problem,
        }
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Substitutes the key into the URL's `{key}` placeholder. A URL without the placeholder is a
/// keyless endpoint and takes no key. The key must be URL-safe (RFC 3986 unreserved characters),
/// so it cannot change the URL's host, path structure, or query.
fn with_key(url: &str, key: Option<&str>) -> Result<String, &'static str> {
    match (url.contains(KEY_PLACEHOLDER), key) {
        (false, None) => Ok(url.to_owned()),
        (false, Some(_)) => Err("is set, but the URL has no {key} placeholder"),
        (true, None) => Err("is required by the {key} placeholder of the URL"),
        (true, Some(key))
            if key.len() >= MIN_KEY_CHARS
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte)) =>
        {
            Ok(url.replace(KEY_PLACEHOLDER, key))
        }
        (true, Some(_)) => Err("must be at least 8 characters of A-Z, a-z, 0-9, and -._~"),
    }
}

/// Returns a log-safe label for the provider entry at `index` in `chain.rpc_providers`.
///
/// Provider ids are used as-is; an inline URL entry is named by its position so its host,
/// path, and credentials never reach an error or log line.
pub(crate) fn provider_label(provider: &str, index: usize) -> String {
    if provider.contains("://") {
        format!("rpc_providers[{index}]")
    } else {
        provider.to_owned()
    }
}

/// Returns the environment-variable name `TOPUP_RPC_<ID>_<SUFFIX>` for one provider id.
fn provider_environment_name(provider_id: &str, suffix: &str) -> String {
    let normalized = provider_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("TOPUP_RPC_{normalized}_{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_match_the_attested_compose_convention() {
        assert_eq!(
            provider_environment_name("provider-a", "URL"),
            "TOPUP_RPC_PROVIDER_A_URL"
        );
        assert_eq!(
            provider_environment_name("quick-node.eu", "KEY"),
            "TOPUP_RPC_QUICK_NODE_EU_KEY"
        );
        assert_eq!(
            configured_provider_url("http://127.0.0.1:8545"),
            Ok("http://127.0.0.1:8545".to_owned())
        );
        assert_eq!(provider_label("provider-a", 0), "provider-a");
        assert_eq!(
            provider_label("https://user:secret@rpc.example/v1?key=secret", 1),
            "rpc_providers[1]"
        );
    }

    #[test]
    fn keys_fill_the_placeholder_where_each_provider_documents_it() {
        let key = "AbC123_-.~xyz";
        for (url, expected) in [
            (
                "https://eth-mainnet.g.alchemy.com/v2/{key}",
                "https://eth-mainnet.g.alchemy.com/v2/AbC123_-.~xyz",
            ),
            (
                "https://name.quiknode.pro/{key}/",
                "https://name.quiknode.pro/AbC123_-.~xyz/",
            ),
            (
                "https://lb.drpc.org/ogrpc?network=ethereum&dkey={key}",
                "https://lb.drpc.org/ogrpc?network=ethereum&dkey=AbC123_-.~xyz",
            ),
        ] {
            assert_eq!(with_key(url, Some(key)).as_deref(), Ok(expected));
        }
        assert_eq!(
            with_key("https://rpc.example/sepolia", None).as_deref(),
            Ok("https://rpc.example/sepolia")
        );
    }

    #[test]
    fn a_key_must_match_its_url() {
        let keyed = "https://rpc.example/v2/{key}";
        assert!(with_key(keyed, None).is_err());
        assert!(with_key("https://rpc.example/sepolia", Some("0123456789abcdef")).is_err());
        // A key that could move the request elsewhere, or too short to be scrubbed from logs.
        for key in [
            "short",
            "0123456789@evil.example",
            "01234567/../x",
            "0123456789?x=1",
        ] {
            assert!(with_key(keyed, Some(key)).is_err(), "{key}");
        }
    }
}
