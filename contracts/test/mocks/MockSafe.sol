// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @dev Mirrors the parts of Safe v1.4.1 that deployment verification reads: slot 0 is reserved
/// for the proxy's singleton address, modules form a sentinel-terminated linked list, and the
/// guard and fallback handler live in Safe's fixed storage slots. Configuration calls must come
/// from the Safe itself, as in Safe's `authorized` modifier.
abstract contract MockSafeBase {
    error NotAuthorized();

    address internal constant SENTINEL_MODULES = address(0x1);
    bytes32 internal constant GUARD_STORAGE_SLOT = keccak256("guard_manager.guard.address");
    bytes32 internal constant FALLBACK_HANDLER_STORAGE_SLOT =
        keccak256("fallback_manager.handler.address");

    address private singleton;
    address[] internal owners;
    mapping(address owner => bool enabled) internal isOwner;
    uint256 internal threshold;
    mapping(address module => address next) private modules;

    modifier authorized() {
        if (msg.sender != address(this)) revert NotAuthorized();
        _;
    }

    function _setup(address[] calldata owners_, uint256 threshold_) internal {
        for (uint256 i; i < owners_.length; ++i) {
            owners.push(owners_[i]);
            isOwner[owners_[i]] = true;
        }
        threshold = threshold_;
        modules[SENTINEL_MODULES] = SENTINEL_MODULES;
    }

    function getOwners() external view returns (address[] memory) {
        return owners;
    }

    function getThreshold() external view returns (uint256) {
        return threshold;
    }

    function enableModule(address module) external authorized {
        modules[module] = modules[SENTINEL_MODULES];
        modules[SENTINEL_MODULES] = module;
    }

    function getModulesPaginated(address start, uint256 pageSize)
        external
        view
        returns (address[] memory array, address next)
    {
        array = new address[](pageSize);
        uint256 count;
        next = modules[start];
        while (next != address(0) && next != SENTINEL_MODULES && count < pageSize) {
            array[count] = next;
            next = modules[next];
            ++count;
        }
        if (next != SENTINEL_MODULES) next = array[count - 1];
        assembly ("memory-safe") {
            mstore(array, count)
        }
    }

    function setGuard(address guard) external authorized {
        bytes32 slot = GUARD_STORAGE_SLOT;
        assembly ("memory-safe") {
            sstore(slot, guard)
        }
    }

    function setFallbackHandler(address handler) external authorized {
        bytes32 slot = FALLBACK_HANDLER_STORAGE_SLOT;
        assembly ("memory-safe") {
            sstore(slot, handler)
        }
    }
}

contract MockSafeSingleton is MockSafeBase {
    error AlreadyInitialized();
    error ExecutionFailed();
    error NotOwner();

    function setup(address[] calldata owners_, uint256 threshold_) external {
        if (owners.length != 0) revert AlreadyInitialized();
        _setup(owners_, threshold_);
    }

    function exec(address target, bytes calldata data) external returns (bytes memory result) {
        if (!isOwner[msg.sender]) revert NotOwner();
        bool success;
        (success, result) = target.call(data);
        if (!success) revert ExecutionFailed();
    }
}

/// @dev Storage-compatible with `MockSafeSingleton` and reports the same owners, threshold,
/// modules, guard, and fallback handler, but lets anyone execute. A genuine proxy pointing at
/// this singleton must be rejected.
contract MaliciousSafeSingleton is MockSafeBase {
    function setup(address[] calldata owners_, uint256 threshold_) external {
        _setup(owners_, threshold_);
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
