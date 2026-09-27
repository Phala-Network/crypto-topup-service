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
