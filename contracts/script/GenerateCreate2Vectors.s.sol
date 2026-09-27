// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Script } from "forge-std/Script.sol";

import { ForwarderFactory } from "../src/ForwarderFactory.sol";

/// Writes `test-vectors/create2.json`: forwarder addresses computed by the factory itself, which
/// the Rust core and the Python SDK must reproduce.
contract GenerateCreate2Vectors is Script {
    struct LockVector {
        string productSlug;
        string externalId;
        string lockRef;
        address treasury;
        bytes32 salt;
        address predictedAddress;
    }

    address private constant DEPLOYER = 0x000000000000000000000000000000000000a11c;
    address private constant TREASURY = 0x0000000000000000000000000000000000007EA5;
    address private constant OTHER_TREASURY = 0x936c1991f8dA9a919fa11b557a3514719f5A4504;
    uint256 private constant SALT_COUNT = 5;

    function run() external returns (ForwarderFactory factory) {
        vm.startBroadcast(DEPLOYER);
        factory = new ForwarderFactory();
        vm.stopBroadcast();

        // The same salts under two treasuries give different addresses.
        address[2] memory treasuries = [TREASURY, OTHER_TREASURY];
        string[] memory forwarders = new string[](SALT_COUNT * treasuries.length);
        for (uint256 t; t < treasuries.length; ++t) {
            for (uint256 i; i < SALT_COUNT; ++i) {
                bytes32 salt = keccak256(abi.encode("crypto-topup-create2-vector", i));
                uint256 index = t * SALT_COUNT + i;
                string memory key = string.concat("forwarder-", vm.toString(index));
                vm.serializeAddress(key, "treasury", treasuries[t]);
                vm.serializeBytes32(key, "salt", salt);
                forwarders[index] = vm.serializeAddress(
                    key, "predicted_address", factory.addressOf(treasuries[t], salt)
                );
            }
        }

        LockVector[3] memory locks = _lockVectors(factory);
        string[] memory lockJson = new string[](locks.length);
        for (uint256 i; i < locks.length; ++i) {
            lockJson[i] = _lockEntryJson(locks[i], i);
        }

        string memory json = string.concat(
            "{\"factory\":\"",
            vm.toString(address(factory)),
            "\",\"implementation\":\"",
            vm.toString(address(factory.implementation())),
            "\",\"forwarders\":",
            _array(forwarders),
            ",\"lock\":",
            _array(lockJson),
            "}"
        );
        vm.writeJson(json, string.concat(vm.projectRoot(), "/test-vectors/create2.json"));
    }

    function _lockVectors(ForwarderFactory factory)
        private
        view
        returns (LockVector[3] memory vectors)
    {
        vectors[0] = _lock(factory, TREASURY, "phala-cloud", "invoice-2026-0001", "checkout-0001");
        vectors[1] =
            _lock(factory, OTHER_TREASURY, "builder", "quote-0042", "rate-lock:builder:0042");
        vectors[2] = _lock(
            factory,
            TREASURY,
            "enterprise",
            "invoice-long-reference",
            string.concat(
                "lock-ref-0000000000000000000000000000000000000000000000000000000000000000-",
                "1111111111111111111111111111111111111111111111111111111111111111-",
                "2222222222222222222222222222222222222222222222222222222222222222-",
                "3333333333333333333333333333333333333333333333333333333333333333"
            )
        );
    }

    function _lock(
        ForwarderFactory factory,
        address treasury,
        string memory productSlug,
        string memory externalId,
        string memory lockRef
    ) private view returns (LockVector memory vector) {
        bytes32 salt = keccak256(abi.encode(productSlug, externalId, "lock", lockRef));
        vector = LockVector({
            productSlug: productSlug,
            externalId: externalId,
            lockRef: lockRef,
            treasury: treasury,
            salt: salt,
            predictedAddress: factory.addressOf(treasury, salt)
        });
    }

    function _lockEntryJson(LockVector memory vector, uint256 index)
        private
        returns (string memory)
    {
        string memory key = string.concat("lock-", vm.toString(index));
        vm.serializeString(key, "product_slug", vector.productSlug);
        vm.serializeString(key, "external_id", vector.externalId);
        vm.serializeString(key, "lock_ref", vector.lockRef);
        vm.serializeAddress(key, "treasury", vector.treasury);
        vm.serializeBytes32(key, "salt", vector.salt);
        return vm.serializeAddress(key, "predicted_address", vector.predictedAddress);
    }

    function _array(string[] memory items) private pure returns (string memory json) {
        json = "[";
        for (uint256 i; i < items.length; ++i) {
            json = string.concat(json, i == 0 ? "" : ",", items[i]);
        }
        json = string.concat(json, "]");
    }
}
