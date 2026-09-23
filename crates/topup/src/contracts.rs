//! Startup verification of the deployed forwarder contracts against the attested routes (§4).

use std::collections::BTreeSet;

use alloy_primitives::{Address, B256, b256, keccak256};
use topup_adapters::chain::flush::ContractAddressGetter;
use topup_core::address::forwarder_address;
use topup_core::route::RouteFile;

use crate::flusher::AlloyChainClient;
use crate::rpc_provider::{
    BALANCE_BATCH_SIZE, RPC_TIMEOUT, configured_provider_url, provider_label,
};

/// `ForwarderFactory` runtime code hash with its `implementation` immutable zeroed, as recorded in
/// `deploy/contracts/expected-codehashes.json`.
const FACTORY_RUNTIME_TEMPLATE_HASH: B256 =
    b256!("3bcaca09de50292271a29ea682474bfef177d16fea1a20ab25092ac34dc8ff1a");

/// `Forwarder` runtime code hash with its `treasury` and `factory` immutables zeroed, as recorded
/// in `deploy/contracts/expected-codehashes.json`.
const FORWARDER_RUNTIME_TEMPLATE_HASH: B256 =
    b256!("0d75243426ce4c396cb01f2586db90ca041e6708235c1a902679efa4f7fbed28");

/// Salt used to compare the factory's `addressOf` with local address derivation.
#[must_use]
pub fn sample_salt() -> B256 {
    keccak256("crypto-topup-service.startup-check")
}

/// Checks every route's contracts on every configured RPC provider before the service starts.
pub async fn verify_routes(routes: &[RouteFile]) -> Result<(), String> {
    let mut checked = BTreeSet::new();
    for route in routes {
        let contracts = &route.chain.contracts;
        for (index, provider) in route.chain.rpc_providers.iter().enumerate() {
            let key = (
                route.chain.chain_id,
                provider.as_str(),
                contracts.forwarder_factory,
                contracts.implementation,
                contracts.treasury,
            );
            if !checked.insert(key) {
                continue;
            }
            let label = provider_label(provider, index);
            let url = configured_provider_url(provider)
                .map_err(|environment| format!("{environment} is required for `{label}`"))?;
            let client =
                AlloyChainClient::connect_http_with_policy(&url, RPC_TIMEOUT, BALANCE_BATCH_SIZE)
                    .map_err(|_| format!("provider `{label}` has an invalid URL"))?
                    .with_provider(label.clone());
            verify_on(&client, route)
                .await
                .map_err(|error| format!("route `{}` via `{label}`: {error}", route.route))?;
        }
    }
    Ok(())
}

/// Compares one provider's view of the contracts with the route. The getters come first so a
/// mismatch names the differing address; the code hashes then prove the immutables are the only
/// difference from the audited build.
async fn verify_on(client: &AlloyChainClient, route: &RouteFile) -> Result<(), String> {
    let contracts = &route.chain.contracts;
    let factory = contracts.forwarder_factory;
    let implementation = contracts.implementation;
    let read = |error: crate::flusher::ChainError| error.to_string();

    let actual = client
        .contract_address(factory, ContractAddressGetter::Implementation)
        .await
        .map_err(read)?;
    if actual != implementation {
        return Err(format!(
            "factory implementation() is {actual:#x}, route expects {implementation:#x}"
        ));
    }
    let treasury = client
        .contract_address(implementation, ContractAddressGetter::Treasury)
        .await
        .map_err(read)?;
    if treasury != contracts.treasury {
        return Err(format!(
            "implementation treasury() is {treasury:#x}, route expects {:#x}",
            contracts.treasury
        ));
    }
    let owner = client
        .contract_address(implementation, ContractAddressGetter::Factory)
        .await
        .map_err(read)?;
    if owner != factory {
        return Err(format!(
            "implementation factory() is {owner:#x}, route expects {factory:#x}"
        ));
    }
    let factory_code = client.code_at(factory).await.map_err(read)?;
    if template_hash(&factory_code, &[implementation]) != FACTORY_RUNTIME_TEMPLATE_HASH {
        return Err(format!(
            "factory {factory:#x} code hash does not match the ForwarderFactory build"
        ));
    }
    let implementation_code = client.code_at(implementation).await.map_err(read)?;
    if template_hash(&implementation_code, &[contracts.treasury, factory])
        != FORWARDER_RUNTIME_TEMPLATE_HASH
    {
        return Err(format!(
            "implementation {implementation:#x} code hash does not match the Forwarder build"
        ));
    }
    let sample = client
        .factory_addresses(factory, &[sample_salt()])
        .await
        .map_err(read)?;
    let expected = forwarder_address(factory, implementation, sample_salt());
    if sample.as_slice() != [expected] {
        return Err(format!(
            "factory addressOf(sample) is {sample:?}, local derivation gives {expected:#x}"
        ));
    }
    Ok(())
}

/// Hashes runtime code after zeroing each 32-byte word that holds one of `immutables`.
///
/// Solidity places address immutables as left-padded words; the build records the code hash with
/// those words zeroed, so the result equals the template hash only when every other byte matches.
fn template_hash(code: &[u8], immutables: &[Address]) -> B256 {
    let mut template = code.to_vec();
    for immutable in immutables {
        let word = immutable.into_word();
        let mut offset = 0;
        while let Some(found) = template.get(offset..).and_then(|rest| {
            rest.windows(32)
                .position(|window| window == word.as_slice())
        }) {
            let start = offset.saturating_add(found);
            let end = start.saturating_add(32);
            if let Some(slot) = template.get_mut(start..end) {
                slot.fill(0);
            }
            offset = end;
        }
    }
    keccak256(template)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_hashes_match_the_recorded_contract_build() {
        let recorded: serde_json::Value = serde_json::from_str(include_str!(
            "../../../deploy/contracts/expected-codehashes.json"
        ))
        .expect("recorded code hashes parse");
        let artifacts = &recorded["artifacts"];
        assert_eq!(
            artifacts["ForwarderFactory"]["runtime_template_code_hash"],
            FACTORY_RUNTIME_TEMPLATE_HASH.to_string()
        );
        assert_eq!(
            artifacts["Forwarder"]["runtime_template_code_hash"],
            FORWARDER_RUNTIME_TEMPLATE_HASH.to_string()
        );
    }

    #[test]
    fn template_hash_zeroes_every_immutable_word() {
        let immutable = Address::repeat_byte(0xab);
        let mut code = vec![0x60, 0x80];
        code.extend_from_slice(immutable.into_word().as_slice());
        code.push(0x5b);
        code.extend_from_slice(immutable.into_word().as_slice());
        let mut template = vec![0x60, 0x80];
        template.extend_from_slice(&[0; 32]);
        template.push(0x5b);
        template.extend_from_slice(&[0; 32]);

        assert_eq!(template_hash(&code, &[immutable]), keccak256(&template));
        assert_ne!(
            template_hash(&code, &[Address::repeat_byte(0xcd)]),
            keccak256(&template)
        );
    }
}
