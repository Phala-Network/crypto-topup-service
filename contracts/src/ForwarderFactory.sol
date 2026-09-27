// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Clones } from "@openzeppelin/contracts/proxy/Clones.sol";
import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {
    ReentrancyGuardTransient
} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";

import { Forwarder } from "./Forwarder.sol";

/// @notice Permissionless factory of deposit forwarders. A forwarder's address commits to this
/// factory, the implementation, its treasury, and its salt; anyone may flush it, and its funds
/// can only ever reach that treasury. There are no roles, no admin, and no constructor arguments.
contract ForwarderFactory is ReentrancyGuardTransient {
    error ZeroTreasury();

    /// Revert data longer than this is truncated in `FlushFailed`.
    uint256 public constant MAX_REASON_LENGTH = 256;

    Forwarder public immutable implementation;

    event ForwarderCreated(
        bytes32 indexed salt, address indexed forwarder, address indexed treasury
    );
    event Flushed(
        bytes32 indexed salt,
        address indexed forwarder,
        address indexed token,
        address treasury,
        uint256 amount
    );
    event FlushFailed(
        bytes32 indexed salt, address indexed forwarder, address indexed token, bytes reason
    );

    constructor() {
        implementation = new Forwarder();
    }

    /// Returns the forwarder address for `treasury` and `salt`, deployed or not.
    function addressOf(address treasury, bytes32 salt) public view returns (address) {
        if (treasury == address(0)) revert ZeroTreasury();
        return Clones.predictDeterministicAddressWithImmutableArgs(
            address(implementation), abi.encodePacked(treasury), salt
        );
    }

    /// Moves the whole `token` balance (ETH when `token` is zero) of each forwarder of `treasury`
    /// named by `salts` to `treasury`. Callable by anyone. A forwarder holding nothing is skipped
    /// and not deployed; one whose transfer fails emits `FlushFailed` and the batch continues.
    function flush(address treasury, bytes32[] calldata salts, address token)
        external
        nonReentrant
    {
        if (treasury == address(0)) revert ZeroTreasury();
        bytes memory args = abi.encodePacked(treasury);
        uint256 length = salts.length;
        for (uint256 i; i < length; ++i) {
            // The batch intentionally flushes one forwarder per salt.
            // forge-lint: disable-next-line(calls-loop)
            _flushSalt(treasury, args, salts[i], token);
        }
    }

    function _flushSalt(address treasury, bytes memory args, bytes32 salt, address token) private {
        address forwarder = Clones.predictDeterministicAddressWithImmutableArgs(
            address(implementation), args, salt
        );
        uint256 balance = forwarder.balance;
        if (token != address(0)) {
            // forge-lint: disable-next-line(calls-loop)
            balance = IERC20(token).balanceOf(forwarder);
        }
        if (balance == 0) return;

        if (forwarder.code.length == 0) {
            Clones.cloneDeterministicWithImmutableArgs(address(implementation), args, salt);
            // forge-lint: disable-next-line(reentrancy-events)
            emit ForwarderCreated(salt, forwarder, treasury);
        }

        (bool success, uint256 amount, bytes memory reason) = _flush(forwarder, token);
        if (success) {
            // The event carries the amount returned by the completed transfer.
            // forge-lint: disable-next-line(reentrancy-events)
            emit Flushed(salt, forwarder, token, treasury, amount);
        } else {
            // forge-lint: disable-next-line(reentrancy-events)
            emit FlushFailed(salt, forwarder, token, reason);
        }
    }

    /// Calls `Forwarder.flush(token)` and copies at most `MAX_REASON_LENGTH` bytes of revert
    /// data, so a failing target cannot make the batch pay for copying a large revert.
    function _flush(address forwarder, address token)
        private
        returns (bool success, uint256 amount, bytes memory reason)
    {
        bytes memory data = abi.encodeCall(Forwarder.flush, (token));
        uint256 size;
        assembly ("memory-safe") {
            success := call(gas(), forwarder, 0, add(data, 0x20), mload(data), 0, 0)
            size := returndatasize()
        }
        // A clone of this factory's implementation returns exactly one word on success.
        if (success && size == 32) {
            amount = abi.decode(_returnData(32), (uint256));
            return (success, amount, reason);
        }
        success = false;
        reason = _returnData(size < MAX_REASON_LENGTH ? size : MAX_REASON_LENGTH);
    }

    function _returnData(uint256 length) private pure returns (bytes memory data) {
        data = new bytes(length);
        assembly ("memory-safe") {
            returndatacopy(add(data, 0x20), 0, length)
        }
    }
}
