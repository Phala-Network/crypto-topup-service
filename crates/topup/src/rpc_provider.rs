//! Shared RPC provider-id configuration.

/// Resolves either an inline HTTP URL or a provider id through its environment variable.
pub(crate) fn configured_provider_url(provider: &str) -> Result<String, String> {
    if provider.contains("://") {
        return Ok(provider.to_owned());
    }
    let environment = provider_environment_name(provider);
    std::env::var(&environment)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(environment)
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

/// Returns the attested environment-variable name for one provider id.
pub(crate) fn provider_environment_name(provider_id: &str) -> String {
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
    format!("TOPUP_RPC_{normalized}_URL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_match_the_attested_compose_convention() {
        assert_eq!(
            provider_environment_name("provider-a"),
            "TOPUP_RPC_PROVIDER_A_URL"
        );
        assert_eq!(
            provider_environment_name("quick-node.eu"),
            "TOPUP_RPC_QUICK_NODE_EU_URL"
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
}
