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

contract MockERC721Transfer {
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);

    function mint(address to, uint256 tokenId) external {
        emit Transfer(address(0), to, tokenId);
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

/// Calls back into a configured target (the factory) from inside `transfer`, and records whether
/// the reentrant call was rejected.
contract ReentrantToken is ERC20 {
    address public target;
    bytes public callback;
    bool public hookAttempted;
    bool public hookRejected;

    constructor() ERC20("Hook Token", "HOOK") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function setCallback(address target_, bytes calldata callback_) external {
        target = target_;
        callback = callback_;
    }

    function transfer(address to, uint256 value) public override returns (bool) {
        if (target != address(0) && !hookAttempted) {
            hookAttempted = true;
            (bool success,) = target.call(callback);
            hookRejected = !success;
        }
        return super.transfer(to, value);
    }
}

/// USDT-style token: `transfer` returns nothing.
contract NoReturnToken {
    mapping(address account => uint256) public balanceOf;
    uint256 public totalSupply;

    function mint(address account, uint256 amount) external {
        balanceOf[account] += amount;
        totalSupply += amount;
    }

    function transfer(address to, uint256 value) external {
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
    }
}

/// Token whose `transfer` returns `false` instead of reverting.
contract FalseReturningToken is ERC20 {
    constructor() ERC20("False Token", "FALSE") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function transfer(address, uint256) public pure override returns (bool) {
        return false;
    }
}

/// Token with an issuer blacklist that blocks both senders and recipients, as USDC and USDT do.
contract BlacklistToken is ERC20 {
    error Blacklisted(address account);

    mapping(address account => bool) public blacklisted;

    constructor() ERC20("Blacklist Token", "BLOCK") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function setBlacklisted(address account, bool value) external {
        blacklisted[account] = value;
    }

    function _update(address from, address to, uint256 value) internal override {
        if (blacklisted[from]) revert Blacklisted(from);
        if (blacklisted[to]) revert Blacklisted(to);
        super._update(from, to, value);
    }
}

/// Token whose `transfer` reverts with `size` bytes of revert data.
contract RevertBombToken is ERC20 {
    uint256 public immutable size;

    constructor(uint256 size_) ERC20("Revert Bomb", "BOMB") {
        size = size_;
    }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function transfer(address, uint256) public view override returns (bool) {
        uint256 length = size;
        assembly ("memory-safe") {
            revert(0, length)
        }
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
