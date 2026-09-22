// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Script } from "forge-std/Script.sol";

import { ForwarderFactory } from "../src/ForwarderFactory.sol";

contract GenerateCreate2Vectors is Script {
    struct PersistentVector {
        string productSlug;
        string externalId;
        uint256 version;
        bytes32 salt;
        address predictedAddress;
    }

    struct LockVector {
        string productSlug;
        string externalId;
        string lockRef;
        bytes32 salt;
        address predictedAddress;
    }

    address private constant DEPLOYER = 0x000000000000000000000000000000000000a11c;
    address private constant ADMIN = 0x000000000000000000000000000000000000AD01;
    address private constant TREASURY = 0x0000000000000000000000000000000000007EA5;

    function run() external returns (ForwarderFactory factory) {
        vm.startBroadcast(DEPLOYER);
        factory = new ForwarderFactory(ADMIN, TREASURY);
        vm.stopBroadcast();

        bytes32[] memory salts = new bytes32[](5);
        address[] memory predicted = new address[](5);
        for (uint256 i; i < salts.length; ++i) {
            salts[i] = keccak256(abi.encode("crypto-topup-create2-vector", i));
            predicted[i] = factory.addressOf(salts[i]);
        }

        string memory objectKey = "create2";
        vm.serializeAddress(objectKey, "factory", address(factory));
        vm.serializeAddress(objectKey, "implementation", address(factory.implementation()));
        vm.serializeBytes32(objectKey, "salts", salts);
        string memory json = vm.serializeAddress(objectKey, "predictedAddresses", predicted);
        string memory path = string.concat(vm.projectRoot(), "/test-vectors/create2.json");
        vm.writeJson(json, path);

        PersistentVector[3] memory persistent = _persistentVectors(factory);
        LockVector[3] memory locks = _lockVectors(factory);
        vm.writeJson(_persistentJson(persistent), path, ".persistent");
        vm.writeJson(_lockJson(locks), path, ".lock");
    }

    function _persistentVectors(ForwarderFactory factory)
        private
        view
        returns (PersistentVector[3] memory vectors)
    {
        vectors[0] = _persistent(factory, "phala-cloud", "account-001", 1);
        vectors[1] = _persistent(factory, "builder", unicode"客户-東京-42", 2);
        vectors[2] = _persistent(factory, "enterprise", "customer/with:delimiters", 42);
    }

    function _lockVectors(ForwarderFactory factory)
        private
        view
        returns (LockVector[3] memory vectors)
    {
        vectors[0] = _lock(factory, "phala-cloud", "invoice-2026-0001", "checkout-0001");
        vectors[1] = _lock(factory, "builder", "quote-0042", "rate-lock:builder:0042");
        vectors[2] = _lock(
            factory,
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

    function _persistent(
        ForwarderFactory factory,
        string memory productSlug,
        string memory externalId,
        uint256 version
    ) private view returns (PersistentVector memory vector) {
        bytes32 salt = keccak256(abi.encode(productSlug, externalId, version));
        vector = PersistentVector({
            productSlug: productSlug,
            externalId: externalId,
            version: version,
            salt: salt,
            predictedAddress: factory.addressOf(salt)
        });
    }

    function _lock(
        ForwarderFactory factory,
        string memory productSlug,
        string memory externalId,
        string memory lockRef
    ) private view returns (LockVector memory vector) {
        bytes32 salt = keccak256(abi.encode(productSlug, externalId, "lock", lockRef));
        vector = LockVector({
            productSlug: productSlug,
            externalId: externalId,
            lockRef: lockRef,
            salt: salt,
            predictedAddress: factory.addressOf(salt)
        });
    }

    function _persistentJson(PersistentVector[3] memory vectors) private returns (string memory) {
        string memory first = _persistentEntryJson(vectors[0], 0);
        string memory second = _persistentEntryJson(vectors[1], 1);
        string memory third = _persistentEntryJson(vectors[2], 2);
        return string.concat("[", first, ",", second, ",", third, "]");
    }

    function _lockJson(LockVector[3] memory vectors) private returns (string memory) {
        string memory first = _lockEntryJson(vectors[0], 0);
        string memory second = _lockEntryJson(vectors[1], 1);
        string memory third = _lockEntryJson(vectors[2], 2);
        return string.concat("[", first, ",", second, ",", third, "]");
    }

    function _persistentEntryJson(PersistentVector memory vector, uint256 index)
        private
        returns (string memory)
    {
        string memory key = string.concat("persistent-", vm.toString(index));
        vm.serializeString(key, "product_slug", vector.productSlug);
        vm.serializeString(key, "external_id", vector.externalId);
        vm.serializeUint(key, "version", vector.version);
        vm.serializeBytes32(key, "salt", vector.salt);
        return vm.serializeAddress(key, "predicted_address", vector.predictedAddress);
    }

    function _lockEntryJson(LockVector memory vector, uint256 index)
        private
        returns (string memory)
    {
        string memory key = string.concat("lock-", vm.toString(index));
        vm.serializeString(key, "product_slug", vector.productSlug);
        vm.serializeString(key, "external_id", vector.externalId);
        vm.serializeString(key, "lock_ref", vector.lockRef);
        vm.serializeBytes32(key, "salt", vector.salt);
        return vm.serializeAddress(key, "predicted_address", vector.predictedAddress);
    }
}
