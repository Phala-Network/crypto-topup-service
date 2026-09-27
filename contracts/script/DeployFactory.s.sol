// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Script } from "forge-std/Script.sol";
import { console2 } from "forge-std/console2.sol";

import { Forwarder } from "../src/Forwarder.sol";
import { ForwarderFactory } from "../src/ForwarderFactory.sol";
import { DeploymentConstants } from "./DeploymentConstants.sol";

/// Deploys the permissionless `ForwarderFactory` through the deterministic deployment proxy. The
/// factory has no constructor arguments, so its address depends only on the build and the salt.
contract DeployFactory is Script {
    error DeploymentFailed();
    error ExistingCodeHashMismatch(address target, bytes32 expected, bytes32 actual);
    error InvalidDeployment(address expected, address actual);
    error MissingExpectedCodeHash(address target);
    error ProxyCodeHashMismatch(bytes32 expected, bytes32 actual);

    function run() external returns (ForwarderFactory factory) {
        bytes memory initCode = type(ForwarderFactory).creationCode;
        address predictedFactory = vm.computeCreate2Address(
            DeploymentConstants.FACTORY_SALT,
            keccak256(initCode),
            DeploymentConstants.DETERMINISTIC_DEPLOYMENT_PROXY
        );
        address predictedImplementation = vm.computeCreateAddress(predictedFactory, 1);

        console2.log(
            "Deterministic deployment proxy", DeploymentConstants.DETERMINISTIC_DEPLOYMENT_PROXY
        );
        console2.log("Factory salt");
        console2.logBytes32(DeploymentConstants.FACTORY_SALT);
        console2.log("Factory init code hash");
        console2.logBytes32(keccak256(initCode));
        console2.log("Predicted factory", predictedFactory);
        console2.log("Predicted implementation", predictedImplementation);

        bytes32 proxyCodeHash = DeploymentConstants.DETERMINISTIC_DEPLOYMENT_PROXY.codehash;
        if (proxyCodeHash != DeploymentConstants.DETERMINISTIC_DEPLOYMENT_PROXY_CODE_HASH) {
            revert ProxyCodeHashMismatch(
                DeploymentConstants.DETERMINISTIC_DEPLOYMENT_PROXY_CODE_HASH, proxyCodeHash
            );
        }

        bytes32 expectedFactoryCodeHash = vm.envOr("EXPECTED_FACTORY_CODE_HASH", bytes32(0));
        bytes32 expectedImplementationCodeHash =
            vm.envOr("EXPECTED_IMPLEMENTATION_CODE_HASH", bytes32(0));

        if (predictedFactory.code.length != 0) {
            _requireCodeHash(predictedFactory, expectedFactoryCodeHash);
            _requireCodeHash(predictedImplementation, expectedImplementationCodeHash);
            factory = ForwarderFactory(predictedFactory);
            _validateDeployment(factory, predictedImplementation);
            return factory;
        }

        // The key comes from the environment, never from forge's command line, so it does not
        // appear in the process list.
        vm.startBroadcast(vm.envUint("PRIVATE_KEY"));
        (bool success, bytes memory result) = DeploymentConstants.DETERMINISTIC_DEPLOYMENT_PROXY
            .call(abi.encodePacked(DeploymentConstants.FACTORY_SALT, initCode));
        vm.stopBroadcast();
        if (!success) revert DeploymentFailed();

        address actualFactory;
        if (result.length == 20) {
            assembly ("memory-safe") {
                actualFactory := shr(96, mload(add(result, 32)))
            }
        }
        if (actualFactory != predictedFactory || predictedFactory.code.length == 0) {
            revert InvalidDeployment(predictedFactory, actualFactory);
        }

        factory = ForwarderFactory(predictedFactory);
        _validateDeployment(factory, predictedImplementation);

        if (expectedFactoryCodeHash != bytes32(0)) {
            _requireCodeHash(predictedFactory, expectedFactoryCodeHash);
        }
        if (expectedImplementationCodeHash != bytes32(0)) {
            _requireCodeHash(predictedImplementation, expectedImplementationCodeHash);
        }
    }

    function _requireCodeHash(address target, bytes32 expected) private view {
        if (expected == bytes32(0)) revert MissingExpectedCodeHash(target);
        bytes32 actual = target.codehash;
        if (actual != expected) revert ExistingCodeHashMismatch(target, expected, actual);
    }

    function _validateDeployment(ForwarderFactory factory, address predictedImplementation)
        private
        view
    {
        address actualImplementation = address(factory.implementation());
        if (actualImplementation != predictedImplementation) {
            revert InvalidDeployment(predictedImplementation, actualImplementation);
        }
        address binding = Forwarder(payable(actualImplementation)).factory();
        if (binding != address(factory)) revert InvalidDeployment(address(factory), binding);
    }
}
