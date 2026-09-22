// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Script } from "forge-std/Script.sol";

import { ForwarderFactory } from "../src/ForwarderFactory.sol";

contract GenerateCreate2Vectors is Script {
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
        vm.writeJson(json, string.concat(vm.projectRoot(), "/test-vectors/create2.json"));
    }
}
