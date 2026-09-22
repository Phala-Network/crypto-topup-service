// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { ERC20 } from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

interface IForwarder {
    function flush(address token) external returns (uint256 amount);
}

contract MockERC20 is ERC20 {
    constructor() ERC20("Mock Token", "MOCK") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }
}

contract FeeOnTransferToken is ERC20 {
    uint256 private constant FEE_DENOMINATOR = 10;

    constructor() ERC20("Fee Token", "FEE") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function _update(address from, address to, uint256 value) internal override {
        if (from == address(0) || to == address(0)) {
            super._update(from, to, value);
            return;
        }

        uint256 fee = value / FEE_DENOMINATOR;
        super._update(from, address(0), fee);
        super._update(from, to, value - fee);
    }
}

contract ReentrantToken is ERC20 {
    bool public hookAttempted;
    bool public hookRejected;

    constructor() ERC20("Hook Token", "HOOK") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function transfer(address to, uint256 value) public override returns (bool) {
        hookAttempted = true;
        try IForwarder(msg.sender).flush(address(this)) returns (uint256) {
            hookRejected = false;
        } catch {
            hookRejected = true;
        }

        return super.transfer(to, value);
    }
}

contract SelectiveRevertingToken is ERC20 {
    error BlockedForwarder(address forwarder);

    address public blockedForwarder;

    constructor() ERC20("Selective Revert Token", "REVERT") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function setBlockedForwarder(address forwarder) external {
        blockedForwarder = forwarder;
    }

    function transfer(address to, uint256 value) public override returns (bool) {
        if (msg.sender == blockedForwarder) revert BlockedForwarder(msg.sender);
        return super.transfer(to, value);
    }
}
