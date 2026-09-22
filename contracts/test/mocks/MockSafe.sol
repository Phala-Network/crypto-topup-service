// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @dev Mirrors Safe's `Singleton` base: slot 0 is reserved for the proxy's singleton address.
contract MockSafeSingleton {
    error AlreadyInitialized();
    error ExecutionFailed();
    error NotOwner();

    address private singleton;
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

/// @dev Storage-compatible with `MockSafeSingleton` and reports the same owners and threshold,
/// but lets anyone execute. A genuine proxy pointing at this singleton must be rejected.
contract MaliciousSafeSingleton {
    address private singleton;
    address[] private owners;
    mapping(address owner => bool enabled) private isOwner;
    uint256 private threshold;

    function setup(address[] calldata owners_, uint256 threshold_) external {
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
        (, result) = target.call(data);
    }
}

/// @dev Mirrors Safe v1.4.1 `SafeProxy`: the singleton lives in slot 0 and `masterCopy()` is
/// answered by the proxy itself. The runtime code does not depend on the singleton.
contract MockSafeProxy {
    address internal singleton;

    constructor(address singleton_, bytes memory initializer) {
        singleton = singleton_;
        (bool success,) = singleton_.delegatecall(initializer);
        require(success);
    }

    fallback() external payable {
        assembly ("memory-safe") {
            let singleton_ := and(sload(0), 0xffffffffffffffffffffffffffffffffffffffff)
            // masterCopy()
            if eq(
                calldataload(0),
                0xa619486e00000000000000000000000000000000000000000000000000000000
            ) {
                mstore(0, singleton_)
                return(0, 0x20)
            }
            calldatacopy(0, 0, calldatasize())
            let success := delegatecall(gas(), singleton_, 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            if iszero(success) { revert(0, returndatasize()) }
            return(0, returndatasize())
        }
    }
}
