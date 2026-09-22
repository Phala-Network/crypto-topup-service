// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Script } from "forge-std/Script.sol";

import { MaliciousSafeSingleton, MockSafeProxy, MockSafeSingleton } from "./mocks/MockSafe.sol";

contract DeployMockSafe is Script {
    /// Deploys a singleton and a Safe proxy owned by `SAFE_OWNER` with threshold 1.
    function run() external returns (MockSafeProxy proxy) {
        vm.startBroadcast();
        proxy = _deployProxy(address(new MockSafeSingleton()));
        vm.stopBroadcast();
    }

    /// Deploys the same proxy code and owners, but pointing at a singleton that ignores them.
    function runMalicious() external returns (MockSafeProxy proxy) {
        vm.startBroadcast();
        proxy = _deployProxy(address(new MaliciousSafeSingleton()));
        vm.stopBroadcast();
    }

    function _deployProxy(address singleton) private returns (MockSafeProxy) {
        address[] memory owners = new address[](1);
        owners[0] = vm.envAddress("SAFE_OWNER");
        return new MockSafeProxy(singleton, abi.encodeCall(MockSafeSingleton.setup, (owners, 1)));
    }
}
