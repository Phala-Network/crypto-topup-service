// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Script } from "forge-std/Script.sol";

import { MockSafeProxy, MockSafeSingleton } from "./mocks/MockSafe.sol";

contract DeployMockSafe is Script {
    function run() external returns (MockSafeProxy proxy) {
        address owner = vm.envAddress("SAFE_OWNER");
        address[] memory owners = new address[](1);
        owners[0] = owner;

        vm.startBroadcast();
        MockSafeSingleton singleton = new MockSafeSingleton();
        proxy = new MockSafeProxy(
            address(singleton), abi.encodeCall(MockSafeSingleton.setup, (owners, 1))
        );
        vm.stopBroadcast();
    }
}
