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

/// Token that misbehaves only for the accounts it is told to, so one target of a batch fails.
contract HostileToken is ERC20 {
    enum Mode {
        Normal,
        BalanceReverts,
        BalanceReturnsShortData,
        BalanceBurnsGas,
        TransferBurnsGas
    }

    error Hostile();

    mapping(address account => Mode) public modeOf;

    constructor() ERC20("Hostile Token", "HOSTILE") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function setMode(address account, Mode mode) external {
        modeOf[account] = mode;
    }

    function balanceOf(address account) public view override returns (uint256) {
        Mode mode = modeOf[account];
        if (mode == Mode.BalanceReverts) revert Hostile();
        if (mode == Mode.BalanceReturnsShortData) {
            assembly ("memory-safe") {
                mstore(0, 1)
                return(0, 31)
            }
        }
        if (mode == Mode.BalanceBurnsGas) _burnGas();
        return super.balanceOf(account);
    }

    function transfer(address to, uint256 value) public override returns (bool) {
        if (modeOf[msg.sender] == Mode.TransferBurnsGas) _burnGas();
        return super.transfer(to, value);
    }

    function _burnGas() private pure {
        assembly ("memory-safe") {
            for { } 1 { } { }
        }
    }
}

interface ITokenReceiver {
    function onTokenReceived(address from, uint256 value) external;
}

/// Token that calls the recipient's `onTokenReceived` hook after a transfer, as ERC-777 and
/// ERC-1363 tokens do, and requires it to succeed.
contract HookToken is ERC20 {
    constructor() ERC20("Hook Receiver Token", "HOOKR") { }

    function mint(address account, uint256 amount) external {
        _mint(account, amount);
    }

    function transfer(address to, uint256 value) public override returns (bool) {
        super.transfer(to, value);
        if (to.code.length != 0) ITokenReceiver(to).onTokenReceived(msg.sender, value);
        return true;
    }
}

/// EIP-1967 upgradeable proxy, as USDC (`FiatTokenProxy`) and USDT0 put their tokens behind.
contract TokenProxy {
    bytes32 private constant IMPLEMENTATION_SLOT =
        0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;

    constructor(address implementation) {
        assembly ("memory-safe") {
            sstore(IMPLEMENTATION_SLOT, implementation)
        }
    }

    fallback() external payable {
        assembly {
            calldatacopy(0, 0, calldatasize())
            let success := delegatecall(gas(), sload(IMPLEMENTATION_SLOT), 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            if iszero(success) { revert(0, returndatasize()) }
            return(0, returndatasize())
        }
    }
}

/// USDC-like implementation (FiatToken v2.1): pausable, with an issuer blacklist checked for
/// sender and recipient in separate storage, and `transfer` returning `true`.
contract UsdcLikeToken {
    event Transfer(address indexed from, address indexed to, uint256 value);

    address public owner;
    bool public paused;
    mapping(address account => bool) public blacklisted;
    mapping(address account => uint256) public balanceOf;
    uint256 public totalSupply;

    function mint(address account, uint256 amount) external {
        balanceOf[account] += amount;
        totalSupply += amount;
        emit Transfer(address(0), account, amount);
    }

    function transfer(address to, uint256 value) external returns (bool) {
        require(!paused, "Pausable: paused");
        require(!blacklisted[msg.sender], "Blacklistable: account is blacklisted");
        require(!blacklisted[to], "Blacklistable: account is blacklisted");
        require(to != address(0), "ERC20: transfer to the zero address");
        require(value <= balanceOf[msg.sender], "ERC20: transfer amount exceeds balance");
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
        return true;
    }
}

/// USDT-like implementation (Ethereum `TetherToken`): `transfer` returns nothing, and checks a
/// deprecation flag, a sender blacklist, and a fee (zero, as on mainnet) before moving funds.
contract UsdtLikeToken {
    event Transfer(address indexed from, address indexed to, uint256 value);

    address public owner;
    bool public deprecated;
    uint256 public basisPointsRate;
    uint256 public maximumFee;
    mapping(address account => bool) public isBlackListed;
    mapping(address account => uint256) public balanceOf;
    uint256 public totalSupply;

    function mint(address account, uint256 amount) external {
        balanceOf[account] += amount;
        totalSupply += amount;
        emit Transfer(address(0), account, amount);
    }

    function transfer(address to, uint256 value) external {
        require(msg.data.length >= 2 * 32 + 4);
        require(!isBlackListed[msg.sender]);
        require(!deprecated);
        uint256 fee = (value * basisPointsRate) / 10_000;
        if (fee > maximumFee) fee = maximumFee;
        uint256 sendAmount = value - fee;
        balanceOf[msg.sender] -= value;
        balanceOf[to] += sendAmount;
        if (fee > 0) {
            balanceOf[owner] += fee;
            emit Transfer(msg.sender, owner, fee);
        }
        emit Transfer(msg.sender, to, sendAmount);
    }
}
