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
    error InsufficientGas();
    error ZeroTreasury();

    /// Revert data longer than this is truncated in `FlushFailed`.
    uint256 public constant MAX_REASON_LENGTH = 256;

    /// Gas forwarded to a token's `balanceOf`, so a token cannot consume a batch's gas. A cold
    /// read through an upgradeable proxy (USDC, USDT0) uses under 8 000.
    uint256 public constant BALANCE_OF_GAS = 30_000;

    /// Gas forwarded to one forwarder's `flush`, so a token or treasury cannot consume a batch's
    /// gas. A cold standard ERC-20 transfer to a new holder, even through an upgradeable proxy
    /// (USDC, USDT0), uses under 75 000, and a native send at most `Forwarder.NATIVE_SEND_GAS`
    /// plus the value transfer and new-account costs; the rest is headroom for gas repricing. A
    /// token whose transfer (with any recipient hook) needs more fails its target.
    uint256 public constant FLUSH_GAS = 200_000;

    /// Gas the factory keeps, beyond the callee's bound, for the call itself: a cold account
    /// access (2 600) and the instructions between the gas check and the call.
    uint256 private constant CALL_GAS_RESERVE = 5000;

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
    /// and not deployed; one whose balance read or transfer fails, or exceeds its gas bound,
    /// emits `FlushFailed` and the batch continues. Reverts with `InsufficientGas` rather than
    /// give a target less than its gas bound.
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
            bool read;
            bytes memory readError;
            (read, balance, readError) =
                _call(token, abi.encodeCall(IERC20.balanceOf, (forwarder)), BALANCE_OF_GAS, true);
            if (!read) {
                // The read was a `staticcall`, which cannot change state.
                // forge-lint: disable-next-line(reentrancy-events)
                emit FlushFailed(salt, forwarder, token, readError);
                return;
            }
        }
        // Zero is deliberately skipped; any positive balance is flushed, with no target equality.
        // slither-disable-next-line incorrect-equality
        if (balance == 0) return;

        if (forwarder.code.length == 0) {
            // The address was computed with the identical implementation, args and salt above.
            // OpenZeppelin reverts if CREATE2 fails; its returned address adds no information.
            // slither-disable-next-line unused-return
            Clones.cloneDeterministicWithImmutableArgs(address(implementation), args, salt);
            // forge-lint: disable-next-line(reentrancy-events)
            emit ForwarderCreated(salt, forwarder, treasury);
        }

        (bool success, uint256 amount, bytes memory reason) =
            _call(forwarder, abi.encodeCall(Forwarder.flush, (token)), FLUSH_GAS, false);
        if (success) {
            // The event carries the amount returned by the completed transfer.
            // forge-lint: disable-next-line(reentrancy-events)
            emit Flushed(salt, forwarder, token, treasury, amount);
        } else {
            // forge-lint: disable-next-line(reentrancy-events)
            emit FlushFailed(salt, forwarder, token, reason);
        }
    }

    /// Calls `target` with exactly `gasLimit` gas (read-only when `isStatic`) and decodes one
    /// word of return data. Fewer than 32 bytes of return data fail the call. On failure at most
    /// `MAX_REASON_LENGTH` bytes of return data are copied, so a failing target cannot make the
    /// batch pay for copying a large revert.
    function _call(address target, bytes memory data, uint256 gasLimit, bool isStatic)
        private
        returns (bool success, uint256 word, bytes memory reason)
    {
        // The callee must receive the whole `gasLimit`: with less (the 63/64 rule), a caller
        // choosing the transaction's gas could make any target fail. Running short reverts the
        // batch instead, as the caller's own error.
        // forge-lint: disable-next-line(require-revert-in-loop)
        if (gasleft() < gasLimit + gasLimit / 63 + CALL_GAS_RESERVE) revert InsufficientGas();
        uint256 size;
        // Call copies no return data; gas is capped and returndata is bounded below.
        // slither-disable-next-line assembly
        assembly ("memory-safe") {
            switch isStatic
            case 0 { success := call(gasLimit, target, 0, add(data, 0x20), mload(data), 0, 0) }
            default {
                success := staticcall(gasLimit, target, add(data, 0x20), mload(data), 0, 0)
            }
            size := returndatasize()
        }
        if (success && size >= 32) {
            word = abi.decode(_returnData(32), (uint256));
            return (success, word, reason);
        }
        success = false;
        reason = _returnData(size < MAX_REASON_LENGTH ? size : MAX_REASON_LENGTH);
    }

    function _returnData(uint256 length) private pure returns (bytes memory data) {
        data = new bytes(length);
        // Call copies no return data; gas is capped and returndata is bounded below.
        // slither-disable-next-line assembly
        assembly ("memory-safe") {
            returndatacopy(add(data, 0x20), 0, length)
        }
    }
}
