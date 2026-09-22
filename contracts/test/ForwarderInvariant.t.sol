// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { StdInvariant } from "forge-std/StdInvariant.sol";
import { Test } from "forge-std/Test.sol";

import { ForwarderFactory } from "../src/ForwarderFactory.sol";
import { MockERC20 } from "./mocks/MockTokens.sol";

contract ForwarderHandler is Test {
    uint256 private constant FORWARDER_COUNT = 8;

    ForwarderFactory public immutable factory;
    MockERC20 public immutable token;

    bytes32[FORWARDER_COUNT] private salts;
    address[FORWARDER_COUNT] private forwarders;

    constructor(ForwarderFactory factory_, MockERC20 token_) {
        factory = factory_;
        token = token_;

        for (uint256 i; i < FORWARDER_COUNT; ++i) {
            bytes32 salt = keccak256(abi.encode("invariant", i));
            salts[i] = salt;
            forwarders[i] = factory_.addressOf(salt);
        }
    }

    function fund(uint256 seed, uint96 amount) external {
        token.mint(forwarders[seed % FORWARDER_COUNT], amount);
    }

    function flush(uint256 seed) external {
        bytes32[] memory batch = new bytes32[](1);
        batch[0] = salts[seed % FORWARDER_COUNT];
        factory.flush(batch, address(token));
    }

    function fundAndFlush(uint256 seed, uint96 amount) external {
        uint256 index = seed % FORWARDER_COUNT;
        token.mint(forwarders[index], amount);

        bytes32[] memory batch = new bytes32[](1);
        batch[0] = salts[index];
        factory.flush(batch, address(token));
    }

    function trackedBalance() external view returns (uint256 total) {
        for (uint256 i; i < FORWARDER_COUNT; ++i) {
            total += token.balanceOf(forwarders[i]);
        }
    }
}

contract ForwarderInvariantTest is StdInvariant, Test {
    address private treasury;
    ForwarderFactory private factory;
    MockERC20 private token;
    ForwarderHandler private handler;

    function setUp() public {
        treasury = makeAddr("treasury");
        factory = new ForwarderFactory(address(this), treasury);
        token = new MockERC20();
        handler = new ForwarderHandler(factory, token);

        factory.grantRole(factory.OPERATOR_ROLE(), address(handler));

        bytes4[] memory selectors = new bytes4[](3);
        selectors[0] = ForwarderHandler.fund.selector;
        selectors[1] = ForwarderHandler.flush.selector;
        selectors[2] = ForwarderHandler.fundAndFlush.selector;
        targetSelector(FuzzSelector({ addr: address(handler), selectors: selectors }));
        targetContract(address(handler));
    }

    function invariant_TokensRemainInForwardersOrReachOnlyTreasury() public view {
        uint256 accounted = token.balanceOf(treasury) + handler.trackedBalance();
        assertEq(token.totalSupply(), accounted);
    }
}
