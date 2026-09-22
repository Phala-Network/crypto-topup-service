// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

contract MockSanctionsOracle {
    mapping(address account => bool sanctioned) private sanctions;

    function setSanctioned(address account, bool sanctioned) external {
        sanctions[account] = sanctioned;
    }

    function isSanctioned(address account) external view returns (bool) {
        return sanctions[account];
    }
}
