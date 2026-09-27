// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// The smallest ERC-20 the checkout pays with: the deployer holds the whole supply.
contract TestToken {
    event Transfer(address indexed from, address indexed to, uint256 value);

    string public constant symbol = "PHA";
    uint8 public constant decimals = 18;
    mapping(address => uint256) public balanceOf;

    constructor(uint256 supply) {
        balanceOf[msg.sender] = supply;
        emit Transfer(address(0), msg.sender, supply);
    }

    function transfer(address to, uint256 value) external returns (bool) {
        require(balanceOf[msg.sender] >= value, "balance");
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
        return true;
    }
}
