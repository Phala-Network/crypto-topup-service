// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

library DeploymentConstants {
    address internal constant DETERMINISTIC_DEPLOYMENT_PROXY =
        0x4e59b44847b379578588920cA78FbF26c0B4956C;
    bytes32 internal constant DETERMINISTIC_DEPLOYMENT_PROXY_CODE_HASH =
        0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989;

    // keccak256("crypto-topup-service.ForwarderFactory.v1")
    bytes32 internal constant FACTORY_SALT =
        0x33f357abc669d0dae6ca878fa2e4435dd82ff4a983efd8a8dd4f2efa9437a426;
}
