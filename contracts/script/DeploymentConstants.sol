// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

library DeploymentConstants {
    address internal constant DETERMINISTIC_DEPLOYMENT_PROXY =
        0x4e59b44847b379578588920cA78FbF26c0B4956C;
    bytes32 internal constant DETERMINISTIC_DEPLOYMENT_PROXY_CODE_HASH =
        0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989;

    // keccak256("phala-pay.ForwarderFactory.v2")
    bytes32 internal constant FACTORY_SALT =
        0x26f1d8427b0c2db52d02ee55402198e592a278fb8541ba4dfaefbd1ea7b09eee;
}
