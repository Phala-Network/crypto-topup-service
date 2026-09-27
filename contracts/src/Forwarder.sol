// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Clones } from "@openzeppelin/contracts/proxy/Clones.sol";
import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import { SafeERC20 } from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

/// @notice Deposit forwarder implementation. Every deposit address is an EIP-1167 clone of this
/// contract whose only immutable argument is its treasury, `abi.encodePacked(treasury)`.
/// A clone can pay only that treasury, and only when its factory asks.
contract Forwarder {
    using SafeERC20 for IERC20;

    error EthTransferFailed();
    error NotAClone();
    error OnlyFactory(address caller);

    /// Gas forwarded with a native transfer, so a treasury cannot consume a batch's gas. It
    /// covers a Safe proxy's `receive` (delegatecall and `SafeReceived` event) with room to spare.
    uint256 public constant NATIVE_SEND_GAS = 50_000;

    /// The factory that deployed this implementation; clones share it through their code.
    address public immutable factory;

    constructor() {
        factory = msg.sender;
    }

    modifier onlyFactory() {
        if (msg.sender != factory) revert OnlyFactory(msg.sender);
        _;
    }

    /// Returns the treasury fixed in this clone's code. Reverts on the implementation itself.
    function treasury() public view returns (address) {
        bytes memory args = Clones.fetchCloneArgs(address(this));
        if (args.length != 20) revert NotAClone();
        // casting to 'bytes20' is safe because the argument is exactly 20 bytes long
        // forge-lint: disable-next-line(unsafe-typecast)
        return address(bytes20(args));
    }

    /// Sends this forwarder's whole balance of `token` (ETH when `token` is zero) to the treasury
    /// and returns the amount that left the forwarder.
    function flush(address token) external onlyFactory returns (uint256 amount) {
        address destination = treasury();
        if (token == address(0)) {
            amount = address(this).balance;
            if (amount == 0) return 0;

            // Assembly so no return data is copied: the send's gas and memory cost stay bounded.
            bool success;
            assembly ("memory-safe") {
                success := call(NATIVE_SEND_GAS, destination, amount, 0, 0, 0, 0)
            }
            if (!success) revert EthTransferFailed();
            return amount;
        }

        amount = IERC20(token).balanceOf(address(this));
        if (amount == 0) return 0;
        IERC20(token).safeTransfer(destination, amount);
    }

    receive() external payable { }
}
