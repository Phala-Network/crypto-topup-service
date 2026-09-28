// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Script } from "forge-std/Script.sol";

import { ForwarderFactory } from "../src/ForwarderFactory.sol";

/// Writes `test-vectors/create2.json`: forwarder addresses computed by the factory itself, which
/// the Rust core and the Python SDK must reproduce.
contract GenerateCreate2Vectors is Script {
    struct QuoteVector {
        string account;
        string clientReferenceId;
        string quoteId;
        address treasury;
        bytes32 salt;
        address predictedAddress;
    }

    struct DepositAddressVector {
        string account;
        bool livemode;
        string clientReferenceId;
        uint256 version;
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

        QuoteVector[3] memory quotes = _quoteVectors(factory);
        string[] memory quoteJson = new string[](quotes.length);
        for (uint256 i; i < quotes.length; ++i) {
            quoteJson[i] = _quoteEntryJson(quotes[i], i);
        }

        DepositAddressVector[4] memory depositAddresses = _depositAddressVectors(factory);
        string[] memory depositAddressJson = new string[](depositAddresses.length);
        for (uint256 i; i < depositAddresses.length; ++i) {
            depositAddressJson[i] = _depositAddressEntryJson(depositAddresses[i], i);
        }

        string memory json = string.concat(
            "{\"factory\":\"",
            vm.toString(address(factory)),
            "\",\"implementation\":\"",
            vm.toString(address(factory.implementation())),
            "\",\"forwarders\":",
            _array(forwarders),
            ",\"quote\":",
            _array(quoteJson),
            ",\"deposit_address\":",
            _array(depositAddressJson),
            "}"
        );
        vm.writeJson(json, string.concat(vm.projectRoot(), "/test-vectors/create2.json"));
    }

    function _quoteVectors(ForwarderFactory factory)
        private
        view
        returns (QuoteVector[3] memory vectors)
    {
        vectors[0] = _quote(
            factory,
            TREASURY,
            "acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10",
            "team-42",
            "qt_5f1c0b6a2d9e4f3a8b7c6d5e4f3a2b10"
        );
        vectors[1] = _quote(
            factory,
            OTHER_TREASURY,
            "acct_9a8b7c6d5e4f40312a1b2c3d4e5f6071",
            "invoice-2026-0001",
            "qt_0f1e2d3c4b5a49687766554433221100"
        );
        vectors[2] = _quote(
            factory,
            TREASURY,
            "acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10",
            string.concat(
                "customer-0000000000000000000000000000000000000000000000000000000000000000-",
                "1111111111111111111111111111111111111111111111111111111111111111-",
                "2222222222222222222222222222222222222222222222222222222222222222-",
                "3333333333333333333333333333333333333333333333333333333333333333"
            ),
            "qt_ffffffffffffffffffffffffffffffff"
        );
    }

    function _quote(
        ForwarderFactory factory,
        address treasury,
        string memory account,
        string memory clientReferenceId,
        string memory quoteId
    ) private view returns (QuoteVector memory vector) {
        bytes32 salt = keccak256(abi.encode(account, clientReferenceId, "quote", quoteId));
        vector = QuoteVector({
            account: account,
            clientReferenceId: clientReferenceId,
            quoteId: quoteId,
            treasury: treasury,
            salt: salt,
            predictedAddress: factory.addressOf(treasury, salt)
        });
    }

    function _quoteEntryJson(QuoteVector memory vector, uint256 index)
        private
        returns (string memory)
    {
        string memory key = string.concat("quote-", vm.toString(index));
        vm.serializeString(key, "account", vector.account);
        vm.serializeString(key, "client_reference_id", vector.clientReferenceId);
        vm.serializeString(key, "quote_id", vector.quoteId);
        vm.serializeAddress(key, "treasury", vector.treasury);
        vm.serializeBytes32(key, "salt", vector.salt);
        return vm.serializeAddress(key, "predicted_address", vector.predictedAddress);
    }

    function _depositAddressVectors(ForwarderFactory factory)
        private
        view
        returns (DepositAddressVector[4] memory vectors)
    {
        // The salt names no chain or asset: one address per customer on every chain whose
        // treasury is the same, and another where the treasury differs (vectors 0 and 1).
        vectors[0] = _depositAddress(
            factory, TREASURY, "acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10", true, "team-42", 1
        );
        vectors[1] = _depositAddress(
            factory, OTHER_TREASURY, "acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10", true, "team-42", 1
        );
        vectors[2] = _depositAddress(
            factory, TREASURY, "acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10", false, "team-42", 2
        );
        vectors[3] = _depositAddress(
            factory,
            TREASURY,
            "acct_ffffffffffffffffffffffffffffffff",
            true,
            unicode"客户 42 with a long reference that spans more than one 32-byte ABI word",
            7
        );
    }

    function _depositAddress(
        ForwarderFactory factory,
        address treasury,
        string memory account,
        bool livemode,
        string memory clientReferenceId,
        uint256 version
    ) private view returns (DepositAddressVector memory vector) {
        bytes32 salt = keccak256(
            abi.encode(account, livemode, clientReferenceId, "deposit_address", version)
        );
        vector = DepositAddressVector({
            account: account,
            livemode: livemode,
            clientReferenceId: clientReferenceId,
            version: version,
            treasury: treasury,
            salt: salt,
            predictedAddress: factory.addressOf(treasury, salt)
        });
    }

    function _depositAddressEntryJson(DepositAddressVector memory vector, uint256 index)
        private
        returns (string memory)
    {
        string memory key = string.concat("deposit-address-", vm.toString(index));
        vm.serializeString(key, "account", vector.account);
        vm.serializeBool(key, "livemode", vector.livemode);
        vm.serializeString(key, "client_reference_id", vector.clientReferenceId);
        vm.serializeUint(key, "version", vector.version);
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
