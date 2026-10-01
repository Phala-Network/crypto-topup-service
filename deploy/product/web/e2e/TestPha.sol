// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// Staging's test PHA in miniature: an ERC-20 whose `mint(address,uint256)` is public.
contract TestPha {
    event Transfer(address indexed from, address indexed to, uint256 value);

    string public constant symbol = "PHA";
    uint8 public constant decimals = 18;
    mapping(address => uint256) public balanceOf;

    function mint(address account, uint256 amount) external {
        balanceOf[account] += amount;
        emit Transfer(address(0), account, amount);
    }

    function transfer(address to, uint256 value) external returns (bool) {
        require(balanceOf[msg.sender] >= value, "balance");
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
        return true;
    }
}

/// Circle's test USDC in miniature: 6 decimals. Its `mint` stays public for the test's payer; the
/// product does not list it as mintable, so the page points to Circle's faucet instead.
contract TestUsdc {
    event Transfer(address indexed from, address indexed to, uint256 value);

    string public constant symbol = "USDC";
    uint8 public constant decimals = 6;
    mapping(address => uint256) public balanceOf;

    function mint(address account, uint256 amount) external {
        balanceOf[account] += amount;
        emit Transfer(address(0), account, amount);
    }

    function transfer(address to, uint256 value) external returns (bool) {
        require(balanceOf[msg.sender] >= value, "balance");
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
        return true;
    }
}

/// Staging's test USDT (Aave's) in miniature, with Tether's Ethereum `transfer`, which returns
/// nothing: 6 decimals. The page mints it through `TestFaucet`, as Aave's faucet mints it.
contract TestUsdt {
    event Transfer(address indexed from, address indexed to, uint256 value);

    string public constant symbol = "USDT";
    uint8 public constant decimals = 6;
    mapping(address => uint256) public balanceOf;

    function mint(address account, uint256 amount) external {
        balanceOf[account] += amount;
        emit Transfer(address(0), account, amount);
    }

    function transfer(address to, uint256 value) external {
        require(balanceOf[msg.sender] >= value, "balance");
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
    }
}

/// Aave's testnet faucet in miniature: its public `mint(token, to, amount)` mints a test token,
/// as staging's test USDT is minted.
contract TestFaucet {
    function mint(address token, address to, uint256 amount) external returns (uint256) {
        TestUsdt(token).mint(to, amount);
        return amount;
    }
}
