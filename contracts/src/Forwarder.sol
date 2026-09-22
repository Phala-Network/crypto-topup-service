// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import { SafeERC20 } from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

contract Forwarder {
    using SafeERC20 for IERC20;

    error EthTransferFailed();
    error OnlyFactory(address caller);
    error TreasuryBalanceDecreased(address token, uint256 beforeBalance, uint256 afterBalance);
    error ZeroAddress();

    address public immutable treasury;
    address public immutable factory;

    constructor(address treasury_, address factory_) {
        if (treasury_ == address(0) || factory_ == address(0)) revert ZeroAddress();

        treasury = treasury_;
        factory = factory_;
    }

    modifier onlyFactory() {
        if (msg.sender != factory) revert OnlyFactory(msg.sender);
        _;
    }

    function flush(address token) external onlyFactory returns (uint256 amount) {
        if (token == address(0)) {
            amount = address(this).balance;
            if (amount == 0) return 0;

            (bool success,) = payable(treasury).call{ value: amount }("");
            if (!success) revert EthTransferFailed();
            return amount;
        }

        IERC20 asset = IERC20(token);
        uint256 balance = asset.balanceOf(address(this));
        if (balance == 0) return 0;

        uint256 beforeBalance = asset.balanceOf(treasury);
        asset.safeTransfer(treasury, balance);
        uint256 afterBalance = asset.balanceOf(treasury);
        if (afterBalance < beforeBalance) {
            revert TreasuryBalanceDecreased(token, beforeBalance, afterBalance);
        }

        return afterBalance - beforeBalance;
    }

    receive() external payable { }
}
