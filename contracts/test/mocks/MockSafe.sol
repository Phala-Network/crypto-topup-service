// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

contract MockSafeSingleton {
    error AlreadyInitialized();
    error ExecutionFailed();
    error NotOwner();

    address[] private owners;
    mapping(address owner => bool enabled) private isOwner;
    uint256 private threshold;

    function setup(address[] calldata owners_, uint256 threshold_) external {
        if (owners.length != 0) revert AlreadyInitialized();
        for (uint256 i; i < owners_.length; ++i) {
            owners.push(owners_[i]);
            isOwner[owners_[i]] = true;
        }
        threshold = threshold_;
    }

    function getOwners() external view returns (address[] memory) {
        return owners;
    }

    function getThreshold() external view returns (uint256) {
        return threshold;
    }

    function exec(address target, bytes calldata data) external returns (bytes memory result) {
        if (!isOwner[msg.sender]) revert NotOwner();
        bool success;
        (success, result) = target.call(data);
        if (!success) revert ExecutionFailed();
    }
}

contract MockSafeProxy {
    bytes32 private constant SINGLETON_SLOT =
        0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;

    constructor(address singleton, bytes memory initializer) {
        assembly ("memory-safe") {
            sstore(SINGLETON_SLOT, singleton)
        }
        (bool success,) = singleton.delegatecall(initializer);
        require(success);
    }

    fallback() external payable {
        bytes32 slot = SINGLETON_SLOT;
        assembly ("memory-safe") {
            let singleton := sload(slot)
            calldatacopy(0, 0, calldatasize())
            let success := delegatecall(gas(), singleton, 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            if iszero(success) { revert(0, returndatasize()) }
            return(0, returndatasize())
        }
    }
}
