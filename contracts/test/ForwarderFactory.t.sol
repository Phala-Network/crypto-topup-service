// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { IAccessControl } from "@openzeppelin/contracts/access/IAccessControl.sol";
import { Test } from "forge-std/Test.sol";

import { Forwarder } from "../src/Forwarder.sol";
import { ForwarderFactory } from "../src/ForwarderFactory.sol";
import {
    FeeOnTransferToken,
    MockERC20,
    ReentrantToken,
    SelectiveRevertingToken
} from "./mocks/MockTokens.sol";

contract RejectingTreasury {
    receive() external payable {
        revert();
    }
}

contract ForwarderFactoryTest is Test {
    event Flushed(
        bytes32 indexed salt, address indexed forwarder, address indexed token, uint256 amount
    );

    address private admin;
    address private operator;
    address private treasury;
    ForwarderFactory private factory;
    MockERC20 private token;

    function setUp() public {
        admin = makeAddr("admin");
        operator = makeAddr("operator");
        treasury = makeAddr("treasury");

        factory = new ForwarderFactory(admin, treasury);
        token = new MockERC20();

        bytes32 operatorRole = factory.OPERATOR_ROLE();
        vm.prank(admin);
        factory.grantRole(operatorRole, operator);
    }

    function test_ImplementationBindsFactoryAndTreasury() public view {
        Forwarder implementation = factory.implementation();

        assertEq(implementation.factory(), address(factory));
        assertEq(implementation.treasury(), treasury);
    }

    function test_PredictionEqualsDeployedAddress() public {
        bytes32 salt = keccak256("prediction");
        address predicted = factory.addressOf(salt);

        _flush(_single(salt), address(token));

        assertGt(predicted.code.length, 0);
        assertEq(factory.addressOf(salt), predicted);
    }

    function test_FlushesFullErc20Balance() public {
        bytes32 salt = keccak256("erc20");
        address forwarder = factory.addressOf(salt);
        uint256 amount = 125 ether;
        token.mint(forwarder, amount);

        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(token), amount);
        _flush(_single(salt), address(token));

        assertEq(token.balanceOf(forwarder), 0);
        assertEq(token.balanceOf(treasury), amount);
    }

    function test_FlushesFullEthBalance() public {
        bytes32 salt = keccak256("eth");
        address forwarder = factory.addressOf(salt);
        uint256 amount = 3 ether;
        _flush(_single(salt), address(token));

        vm.deal(address(this), amount);
        (bool success,) = payable(forwarder).call{ value: amount }("");
        assertTrue(success);

        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(0), amount);
        _flush(_single(salt), address(0));

        assertEq(forwarder.balance, 0);
        assertEq(treasury.balance, amount);
    }

    function test_EthFlushChecksTreasuryCallSuccess() public {
        RejectingTreasury rejectingTreasury = new RejectingTreasury();
        ForwarderFactory rejectingFactory =
            new ForwarderFactory(address(this), address(rejectingTreasury));
        rejectingFactory.grantRole(rejectingFactory.OPERATOR_ROLE(), operator);

        bytes32 salt = keccak256("rejecting-treasury");
        address forwarder = rejectingFactory.addressOf(salt);
        vm.deal(forwarder, 1 ether);
        bytes memory ethError = abi.encodeWithSelector(Forwarder.EthTransferFailed.selector);

        vm.prank(operator);
        vm.expectRevert(
            abi.encodeWithSelector(
                ForwarderFactory.ForwarderFlushFailed.selector, salt, forwarder, ethError
            )
        );
        rejectingFactory.flush(_single(salt), address(0));

        assertEq(forwarder.balance, 1 ether);
        assertEq(address(rejectingTreasury).balance, 0);
    }

    function test_ForwarderRejectsNonFactoryCaller() public {
        Forwarder implementation = factory.implementation();

        vm.expectRevert(abi.encodeWithSelector(Forwarder.OnlyFactory.selector, address(this)));
        implementation.flush(address(token));
    }

    function test_FactoryRejectsNonOperator() public {
        bytes32[] memory salts = _single(keccak256("unauthorized"));

        vm.expectRevert(
            abi.encodeWithSelector(
                IAccessControl.AccessControlUnauthorizedAccount.selector,
                address(this),
                factory.OPERATOR_ROLE()
            )
        );
        factory.flush(salts, address(token));
    }

    function test_BatchFlushesMixedDeployedAndUndeployedForwarders() public {
        bytes32 deployedSalt = keccak256("deployed");
        bytes32 undeployedSalt = keccak256("undeployed");
        address deployed = factory.addressOf(deployedSalt);
        address undeployed = factory.addressOf(undeployedSalt);

        _flush(_single(deployedSalt), address(token));
        token.mint(deployed, 4 ether);
        token.mint(undeployed, 7 ether);

        bytes32[] memory salts = new bytes32[](2);
        salts[0] = deployedSalt;
        salts[1] = undeployedSalt;
        _flush(salts, address(token));

        assertGt(deployed.code.length, 0);
        assertGt(undeployed.code.length, 0);
        assertEq(token.balanceOf(deployed), 0);
        assertEq(token.balanceOf(undeployed), 0);
        assertEq(token.balanceOf(treasury), 11 ether);
    }

    function test_ReflushEmptyForwarderEmitsZero() public {
        bytes32 salt = keccak256("empty");
        address forwarder = factory.addressOf(salt);
        _flush(_single(salt), address(token));

        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(token), 0);
        _flush(_single(salt), address(token));

        assertEq(token.balanceOf(treasury), 0);
    }

    function test_FailingForwarderRevertsEntireBatchWithContext() public {
        SelectiveRevertingToken revertingToken = new SelectiveRevertingToken();
        bytes32 firstSalt = keccak256("first");
        bytes32 failingSalt = keccak256("failing");
        address first = factory.addressOf(firstSalt);
        address failing = factory.addressOf(failingSalt);

        bytes32[] memory salts = new bytes32[](2);
        salts[0] = firstSalt;
        salts[1] = failingSalt;
        _flush(salts, address(token));

        revertingToken.mint(first, 5 ether);
        revertingToken.mint(failing, 8 ether);
        revertingToken.setBlockedForwarder(failing);

        bytes memory tokenError =
            abi.encodeWithSelector(SelectiveRevertingToken.BlockedForwarder.selector, failing);
        vm.prank(operator);
        vm.expectRevert(
            abi.encodeWithSelector(
                ForwarderFactory.ForwarderFlushFailed.selector, failingSalt, failing, tokenError
            )
        );
        factory.flush(salts, address(revertingToken));

        assertEq(revertingToken.balanceOf(first), 5 ether);
        assertEq(revertingToken.balanceOf(failing), 8 ether);
        assertEq(revertingToken.balanceOf(treasury), 0);
    }

    function test_ReentrantTokenCannotRedirectFunds() public {
        ReentrantToken hookToken = new ReentrantToken();
        bytes32 salt = keccak256("reentrant");
        address forwarder = factory.addressOf(salt);
        uint256 amount = 19 ether;
        hookToken.mint(forwarder, amount);

        _flush(_single(salt), address(hookToken));

        assertTrue(hookToken.hookAttempted());
        assertTrue(hookToken.hookRejected());
        assertEq(hookToken.balanceOf(forwarder), 0);
        assertEq(hookToken.balanceOf(treasury), amount);
    }

    function test_FeeOnTransferEventReportsAmountReceivedByTreasury() public {
        FeeOnTransferToken feeToken = new FeeOnTransferToken();
        bytes32 salt = keccak256("fee-on-transfer");
        address forwarder = factory.addressOf(salt);
        uint256 sent = 100 ether;
        uint256 received = 90 ether;
        feeToken.mint(forwarder, sent);

        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(feeToken), received);
        _flush(_single(salt), address(feeToken));

        assertEq(feeToken.balanceOf(forwarder), 0);
        assertEq(feeToken.balanceOf(treasury), received);
    }

    function testFuzz_FlushesFullTokenBalance(bytes32 salt, uint96 rawAmount) public {
        uint256 amount = bound(uint256(rawAmount), 1, type(uint96).max);
        address forwarder = factory.addressOf(salt);
        token.mint(forwarder, amount);

        _flush(_single(salt), address(token));

        assertEq(token.balanceOf(forwarder), 0);
        assertEq(token.balanceOf(treasury), amount);
    }

    function _flush(bytes32[] memory salts, address asset) private {
        vm.prank(operator);
        factory.flush(salts, asset);
    }

    function _single(bytes32 salt) private pure returns (bytes32[] memory salts) {
        salts = new bytes32[](1);
        salts[0] = salt;
    }
}
