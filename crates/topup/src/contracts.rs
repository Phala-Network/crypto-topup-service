//! Startup verification of the deployed forwarder contracts against the attested routes (§4).

use std::collections::BTreeSet;

use alloy_primitives::{Address, B256, b256, keccak256};
use topup_adapters::chain::flush::ContractAddressGetter;
use topup_core::address::forwarder_address;
use topup_core::route::RouteFile;

use topup_adapters::chain::evm::{ChainError, EvmClient};

use crate::routes::RouteSet;

// The build fingerprints below are recorded by `deploy/contracts/check-build.sh --write` in
// `deploy/contracts/expected-codehashes.json`; a unit test keeps these copies equal to that file,
// which the service image does not contain.

/// `ForwarderFactory` runtime code hash with its immutable words zeroed.
const FACTORY_RUNTIME_TEMPLATE_HASH: B256 =
    b256!("7abfec3082d6440476f52c9cc04b271dfe4b802e47feeb39f69670253523f0ee");

/// Byte offsets of the 32-byte `implementation` words in `ForwarderFactory` runtime code.
const FACTORY_IMPLEMENTATION_OFFSETS: &[usize] = &[303, 750, 1150];

/// `Forwarder` runtime code hash with its immutable words zeroed.
const FORWARDER_RUNTIME_TEMPLATE_HASH: B256 =
    b256!("1fa01f51e22b763abd13dfc194dd43ee02355f68b534985877bf6b08de7e2029");

/// Byte offsets of the 32-byte `treasury` words in `Forwarder` runtime code.
const FORWARDER_TREASURY_OFFSETS: &[usize] = &[82, 356, 650, 783, 843];

/// Byte offsets of the 32-byte `factory` words in `Forwarder` runtime code.
const FORWARDER_FACTORY_OFFSETS: &[usize] = &[207, 253];

/// Salt used to compare the factory's `addressOf` with local address derivation.
#[must_use]
pub fn sample_salt() -> B256 {
    keccak256("crypto-topup-service.startup-check")
}

/// Checks every route's contracts on every configured RPC provider before the service starts.
pub async fn verify_routes(routes: &RouteSet) -> Result<(), String> {
    let mut checked = BTreeSet::new();
    for route in routes.routes() {
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
            let client = routes
                .provider(route.chain.chain_id, index)
                .map_err(|error| error.to_string())?;
            let label = client.endpoint().provider().unwrap_or_default();
            verify_on(client, route)
                .await
                .map_err(|error| format!("route `{}` via `{label}`: {error}", route.route))?;
        }
    }
    Ok(())
}

/// Compares one provider's view of the contracts with the route. The getters come first so a
/// mismatch names the differing address; the code hashes then prove the immutables are the only
/// difference from the audited build.
async fn verify_on(client: &EvmClient, route: &RouteFile) -> Result<(), String> {
    let contracts = &route.chain.contracts;
    let factory = contracts.forwarder_factory;
    let implementation = contracts.implementation;
    let read = |error: ChainError| error.to_string();

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
    verify_code(
        &factory_code,
        &[(implementation, FACTORY_IMPLEMENTATION_OFFSETS)],
        FACTORY_RUNTIME_TEMPLATE_HASH,
    )
    .map_err(|error| format!("factory {factory:#x} {error} of the ForwarderFactory build"))?;
    let implementation_code = client.code_at(implementation).await.map_err(read)?;
    verify_code(
        &implementation_code,
        &[
            (contracts.treasury, FORWARDER_TREASURY_OFFSETS),
            (factory, FORWARDER_FACTORY_OFFSETS),
        ],
        FORWARDER_RUNTIME_TEMPLATE_HASH,
    )
    .map_err(|error| {
        format!("implementation {implementation:#x} {error} of the Forwarder build")
    })?;
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

/// Checks runtime code against a build whose immutable words are zeroed.
///
/// Each immutable word must hold the route's expected address at exactly the recorded offsets;
/// after zeroing those words, the code must hash to the recorded template hash.
fn verify_code(
    code: &[u8],
    immutables: &[(Address, &[usize])],
    template_hash: B256,
) -> Result<(), &'static str> {
    let mut template = code.to_vec();
    for (expected, offsets) in immutables {
        let word = expected.into_word();
        for offset in *offsets {
            let slot = offset
                .checked_add(32)
                .and_then(|end| template.get_mut(*offset..end))
                .ok_or("runtime code is shorter than the immutable references")?;
            if slot != word.as_slice() {
                return Err("does not hold the route's addresses in the immutable words");
            }
            slot.fill(0);
        }
    }
    if keccak256(&template) != template_hash {
        return Err("runtime code differs from the recorded code hash");
    }
    Ok(())
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
    fn immutable_offsets_match_the_recorded_contract_build() {
        let recorded: serde_json::Value = serde_json::from_str(include_str!(
            "../../../deploy/contracts/expected-codehashes.json"
        ))
        .expect("recorded code hashes parse");
        let offsets = |contract: &str, immutable: &str| -> Vec<usize> {
            serde_json::from_value(
                recorded["artifacts"][contract]["immutable_offsets"][immutable].clone(),
            )
            .expect("recorded offsets are a list")
        };
        assert_eq!(
            offsets("ForwarderFactory", "implementation"),
            FACTORY_IMPLEMENTATION_OFFSETS
        );
        assert_eq!(offsets("Forwarder", "treasury"), FORWARDER_TREASURY_OFFSETS);
        assert_eq!(offsets("Forwarder", "factory"), FORWARDER_FACTORY_OFFSETS);
    }

    #[test]
    fn code_check_uses_exact_immutable_offsets() {
        let immutable = Address::repeat_byte(0xab);
        let mut template = vec![0x60; 70];
        template[2..34].fill(0);
        let hash = keccak256(&template);
        let mut code = template.clone();
        code[2..34].copy_from_slice(immutable.into_word().as_slice());
        let offsets: &[usize] = &[2];

        assert_eq!(verify_code(&code, &[(immutable, offsets)], hash), Ok(()));
        // The same address at another offset, a different address, or trailing code all fail.
        let mut moved = template.clone();
        moved[36..68].copy_from_slice(immutable.into_word().as_slice());
        assert!(verify_code(&moved, &[(immutable, offsets)], hash).is_err());
        assert!(verify_code(&code, &[(Address::repeat_byte(0xcd), offsets)], hash).is_err());
        let mut extended = code.clone();
        extended.push(0);
        assert!(verify_code(&extended, &[(immutable, offsets)], hash).is_err());
        assert!(verify_code(&code[..20], &[(immutable, offsets)], hash).is_err());
    }
}
