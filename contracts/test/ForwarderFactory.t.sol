// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Clones } from "@openzeppelin/contracts/proxy/Clones.sol";
import { SafeERC20 } from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import { Test, Vm } from "forge-std/Test.sol";

import { Forwarder } from "../src/Forwarder.sol";
import { ForwarderFactory } from "../src/ForwarderFactory.sol";
import {
    DelegatingReceiver,
    EventReceiverSingleton,
    GasGriefingTreasury,
    ReentrantTreasury,
    RejectingTreasury
} from "./mocks/MockReceivers.sol";
import {
    BlacklistToken,
    FalseReturningToken,
    FeeOnTransferToken,
    MockERC20,
    NoReturnToken,
    ReentrantToken,
    RevertBombToken
} from "./mocks/MockTokens.sol";

contract ForwarderFactoryTest is Test {
    event ForwarderCreated(
        bytes32 indexed salt, address indexed forwarder, address indexed treasury
    );
    event Flushed(
        bytes32 indexed salt,
        address indexed forwarder,
        address indexed token,
        address treasury,
        uint256 amount
    );
    event FlushFailed(
        bytes32 indexed salt, address indexed forwarder, address indexed token, bytes reason
    );

    address private treasury;
    address private caller;
    ForwarderFactory private factory;
    MockERC20 private token;

    function setUp() public {
        treasury = makeAddr("treasury");
        caller = makeAddr("anyone");
        factory = new ForwarderFactory();
        token = new MockERC20();
    }

    // Deployment and address prediction

    function test_ImplementationIsBoundToFactoryAndIsNotAClone() public {
        Forwarder implementation = factory.implementation();

        assertEq(implementation.factory(), address(factory));
        vm.expectRevert(Forwarder.NotAClone.selector);
        implementation.treasury();
        vm.expectRevert(abi.encodeWithSelector(Forwarder.OnlyFactory.selector, address(this)));
        implementation.flush(address(token));
    }

    function test_PredictionEqualsDeployedCloneWithTreasuryArgument() public {
        bytes32 salt = keccak256("prediction");
        address predicted = factory.addressOf(treasury, salt);
        token.mint(predicted, 1 ether);

        vm.expectEmit(true, true, true, true, address(factory));
        emit ForwarderCreated(salt, predicted, treasury);
        _flush(salt, address(token));

        assertGt(predicted.code.length, 0);
        assertEq(Forwarder(payable(predicted)).treasury(), treasury);
        assertEq(Forwarder(payable(predicted)).factory(), address(factory));
        assertEq(Clones.fetchCloneArgs(predicted), abi.encodePacked(treasury));
    }

    function test_AddressCommitsToTreasury() public {
        bytes32 salt = keccak256("same-salt");
        address other = makeAddr("other-treasury");

        assertTrue(factory.addressOf(treasury, salt) != factory.addressOf(other, salt));
    }

    function test_ZeroTreasuryIsRefused() public {
        bytes32[] memory salts = _single(keccak256("zero"));

        vm.expectRevert(ForwarderFactory.ZeroTreasury.selector);
        factory.addressOf(address(0), salts[0]);
        vm.expectRevert(ForwarderFactory.ZeroTreasury.selector);
        factory.flush(address(0), salts, address(token));
    }

    function test_ZeroBalanceTargetDeploysNothingAndEmitsNothing() public {
        bytes32 salt = keccak256("empty");
        address forwarder = factory.addressOf(treasury, salt);

        vm.recordLogs();
        _flush(salt, address(token));
        _flush(salt, address(0));

        assertEq(vm.getRecordedLogs().length, 0);
        assertEq(forwarder.code.length, 0);
    }

    function test_ForwarderRejectsNonFactoryCaller() public {
        bytes32 salt = keccak256("direct");
        address forwarder = factory.addressOf(treasury, salt);
        token.mint(forwarder, 1 ether);
        _flush(salt, address(token));
        token.mint(forwarder, 1 ether);

        vm.prank(caller);
        vm.expectRevert(abi.encodeWithSelector(Forwarder.OnlyFactory.selector, caller));
        Forwarder(payable(forwarder)).flush(address(token));
    }

    // Transfers

    function test_AnyoneFlushesFullErc20BalanceToTheTreasury() public {
        bytes32 salt = keccak256("erc20");
        address forwarder = factory.addressOf(treasury, salt);
        uint256 amount = 125 ether;
        token.mint(forwarder, amount);

        vm.expectEmit(true, true, true, true, address(factory));
        emit ForwarderCreated(salt, forwarder, treasury);
        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(token), treasury, amount);
        _flush(salt, address(token));

        assertEq(token.balanceOf(forwarder), 0);
        assertEq(token.balanceOf(treasury), amount);
        assertEq(token.balanceOf(caller), 0);
    }

    function test_DeployedForwarderIsFlushedAgainWithoutRedeployment() public {
        bytes32 salt = keccak256("again");
        address forwarder = factory.addressOf(treasury, salt);
        token.mint(forwarder, 1 ether);
        _flush(salt, address(token));
        token.mint(forwarder, 2 ether);

        vm.recordLogs();
        _flush(salt, address(token));

        Vm.Log[] memory logs = vm.getRecordedLogs();
        // The token's Transfer and the factory's Flushed; no ForwarderCreated.
        assertEq(logs.length, 2);
        assertEq(logs[1].topics[0], Flushed.selector);
        assertEq(token.balanceOf(treasury), 3 ether);
    }

    function test_FlushesFullEthBalance() public {
        bytes32 salt = keccak256("eth");
        address forwarder = factory.addressOf(treasury, salt);
        uint256 amount = 3 ether;
        vm.deal(address(this), amount);
        (bool success,) = payable(forwarder).call{ value: amount }("");
        assertTrue(success);

        vm.expectEmit(true, true, true, true, address(factory));
        emit ForwarderCreated(salt, forwarder, treasury);
        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(0), treasury, amount);
        _flush(salt, address(0));

        assertEq(forwarder.balance, 0);
        assertEq(treasury.balance, amount);

        // A deployed clone accepts plain ETH transfers too.
        vm.deal(address(this), 1 ether);
        (success,) = payable(forwarder).call{ value: 1 ether }("");
        assertTrue(success);
        _flush(salt, address(0));
        assertEq(treasury.balance, amount + 1 ether);
    }

    function test_EthReachesAProxyTreasuryWithinTheGasBound() public {
        address proxyTreasury =
            address(new DelegatingReceiver(address(new EventReceiverSingleton())));
        bytes32 salt = keccak256("safe-like");
        address forwarder = factory.addressOf(proxyTreasury, salt);
        vm.deal(forwarder, 2 ether);

        factory.flush(proxyTreasury, _single(salt), address(0));

        assertEq(proxyTreasury.balance, 2 ether);
        assertEq(DelegatingReceiver(payable(proxyTreasury)).received(), 2 ether);
    }

    function test_BatchFlushesMixedDeployedAndUndeployedForwarders() public {
        bytes32 deployedSalt = keccak256("deployed");
        bytes32 undeployedSalt = keccak256("undeployed");
        address deployed = factory.addressOf(treasury, deployedSalt);
        address undeployed = factory.addressOf(treasury, undeployedSalt);
        token.mint(deployed, 1 ether);
        _flush(deployedSalt, address(token));
        token.mint(deployed, 4 ether);
        token.mint(undeployed, 7 ether);

        _flushBatch(_pair(deployedSalt, undeployedSalt), address(token));

        assertGt(undeployed.code.length, 0);
        assertEq(token.balanceOf(deployed), 0);
        assertEq(token.balanceOf(undeployed), 0);
        assertEq(token.balanceOf(treasury), 12 ether);
    }

    function test_WrongTreasuryArgumentCannotReachAnotherTreasurysForwarder() public {
        bytes32 salt = keccak256("mine");
        address forwarder = factory.addressOf(treasury, salt);
        token.mint(forwarder, 5 ether);
        address attacker = makeAddr("attacker");

        vm.recordLogs();
        vm.prank(attacker);
        factory.flush(attacker, _single(salt), address(token));

        assertEq(vm.getRecordedLogs().length, 0);
        assertEq(token.balanceOf(forwarder), 5 ether);
        assertEq(token.balanceOf(attacker), 0);
    }

    // Failure isolation

    function test_BlacklistedForwarderFailsAloneInTheBatch() public {
        BlacklistToken blacklist = new BlacklistToken();
        bytes32 firstSalt = keccak256("first");
        bytes32 blockedSalt = keccak256("blocked");
        address first = factory.addressOf(treasury, firstSalt);
        address blocked = factory.addressOf(treasury, blockedSalt);
        blacklist.mint(first, 5 ether);
        blacklist.mint(blocked, 8 ether);
        blacklist.setBlacklisted(blocked, true);

        bytes memory reason = abi.encodeWithSelector(BlacklistToken.Blacklisted.selector, blocked);
        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(firstSalt, first, address(blacklist), treasury, 5 ether);
        vm.expectEmit(true, true, true, true, address(factory));
        emit FlushFailed(blockedSalt, blocked, address(blacklist), reason);
        _flushBatch(_pair(firstSalt, blockedSalt), address(blacklist));

        assertEq(blacklist.balanceOf(treasury), 5 ether);
        assertEq(blacklist.balanceOf(blocked), 8 ether);
        // The failed forwarder was deployed and can be flushed once unblocked.
        blacklist.setBlacklisted(blocked, false);
        _flush(blockedSalt, address(blacklist));
        assertEq(blacklist.balanceOf(treasury), 13 ether);
    }

    function test_BlacklistedTreasuryFailsWithoutRevertingTheTransaction() public {
        BlacklistToken blacklist = new BlacklistToken();
        bytes32 salt = keccak256("blacklisted-treasury");
        address forwarder = factory.addressOf(treasury, salt);
        blacklist.mint(forwarder, 1 ether);
        blacklist.setBlacklisted(treasury, true);

        vm.expectEmit(true, true, true, true, address(factory));
        emit FlushFailed(
            salt,
            forwarder,
            address(blacklist),
            abi.encodeWithSelector(BlacklistToken.Blacklisted.selector, treasury)
        );
        _flush(salt, address(blacklist));

        assertEq(blacklist.balanceOf(forwarder), 1 ether);
    }

    function test_RevertingTreasuryFailsOnlyItsOwnTargets() public {
        address rejecting = address(new RejectingTreasury());
        bytes32 salt = keccak256("rejecting");
        address failing = factory.addressOf(rejecting, salt);
        address working = factory.addressOf(treasury, salt);
        vm.deal(failing, 1 ether);
        vm.deal(working, 2 ether);

        vm.expectEmit(true, true, true, true, address(factory));
        emit FlushFailed(
            salt, failing, address(0), abi.encodeWithSelector(Forwarder.EthTransferFailed.selector)
        );
        factory.flush(rejecting, _single(salt), address(0));
        _flush(salt, address(0));

        assertEq(failing.balance, 1 ether);
        assertEq(rejecting.balance, 0);
        assertEq(treasury.balance, 2 ether);
    }

    function test_GasGriefingTreasuryCannotConsumeTheBatchGas() public {
        address griefer = address(new GasGriefingTreasury());
        uint256 targets = 10;
        bytes32[] memory salts = new bytes32[](targets);
        for (uint256 i; i < targets; ++i) {
            salts[i] = keccak256(abi.encode("grief", i));
            vm.deal(factory.addressOf(griefer, salts[i]), 1 ether);
        }

        vm.recordLogs();
        uint256 before = gasleft();
        factory.flush{ gas: 2_000_000 }(griefer, salts, address(0));
        uint256 used = before - gasleft();

        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 failures;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == FlushFailed.selector) ++failures;
        }
        assertEq(failures, targets);
        // Each target costs a clone deployment plus at most NATIVE_SEND_GAS in the treasury.
        assertLt(used, targets * 150_000);
        assertEq(griefer.balance, 0);
    }

    function test_RevertDataIsTruncated() public {
        RevertBombToken bomb = new RevertBombToken(10_000);
        bytes32 salt = keccak256("bomb");
        address forwarder = factory.addressOf(treasury, salt);
        bomb.mint(forwarder, 1 ether);

        vm.recordLogs();
        _flush(salt, address(bomb));

        Vm.Log[] memory logs = vm.getRecordedLogs();
        Vm.Log memory failed = logs[logs.length - 1];
        assertEq(failed.topics[0], FlushFailed.selector);
        bytes memory reason = abi.decode(failed.data, (bytes));
        assertEq(reason.length, factory.MAX_REASON_LENGTH());
    }

    // ERC-20 edge cases

    function test_NoReturnTokenIsFlushed() public {
        NoReturnToken usdtLike = new NoReturnToken();
        bytes32 salt = keccak256("no-return");
        address forwarder = factory.addressOf(treasury, salt);
        usdtLike.mint(forwarder, 42e6);

        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(usdtLike), treasury, 42e6);
        _flush(salt, address(usdtLike));

        assertEq(usdtLike.balanceOf(forwarder), 0);
        assertEq(usdtLike.balanceOf(treasury), 42e6);
    }

    function test_FalseReturningTokenFails() public {
        FalseReturningToken falseToken = new FalseReturningToken();
        bytes32 salt = keccak256("false");
        address forwarder = factory.addressOf(treasury, salt);
        falseToken.mint(forwarder, 1 ether);

        vm.expectEmit(true, true, true, true, address(factory));
        emit FlushFailed(
            salt,
            forwarder,
            address(falseToken),
            abi.encodeWithSelector(SafeERC20.SafeERC20FailedOperation.selector, address(falseToken))
        );
        _flush(salt, address(falseToken));
    }

    /// Fee-on-transfer tokens are unsupported (routes must not enable them). The documented
    /// behavior: `Flushed.amount` is what left the forwarder; the treasury receives it minus the
    /// token's fee, and nothing reaches any other address the forwarder chooses.
    function test_FeeOnTransferReportsTheAmountThatLeftTheForwarder() public {
        FeeOnTransferToken feeToken = new FeeOnTransferToken();
        bytes32 salt = keccak256("fee-on-transfer");
        address forwarder = factory.addressOf(treasury, salt);
        feeToken.mint(forwarder, 100 ether);

        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(feeToken), treasury, 100 ether);
        _flush(salt, address(feeToken));

        assertEq(feeToken.balanceOf(forwarder), 0);
        assertEq(feeToken.balanceOf(treasury), 90 ether);
    }

    // Reentrancy

    function test_ReentrantTokenCannotReenterTheFactory() public {
        ReentrantToken hookToken = new ReentrantToken();
        bytes32 salt = keccak256("reentrant");
        address forwarder = factory.addressOf(treasury, salt);
        hookToken.mint(forwarder, 19 ether);
        hookToken.setCallback(
            address(factory),
            abi.encodeCall(ForwarderFactory.flush, (treasury, _single(salt), address(hookToken)))
        );

        vm.expectEmit(true, true, true, true, address(factory));
        emit Flushed(salt, forwarder, address(hookToken), treasury, 19 ether);
        _flush(salt, address(hookToken));

        assertTrue(hookToken.hookAttempted());
        assertTrue(hookToken.hookRejected());
        assertEq(hookToken.balanceOf(forwarder), 0);
        assertEq(hookToken.balanceOf(treasury), 19 ether);
    }

    function test_ReentrantTreasuryCannotReenterTheFactory() public {
        ReentrantTreasury hookTreasury = new ReentrantTreasury();
        bytes32 salt = keccak256("reentrant-treasury");
        address forwarder = factory.addressOf(address(hookTreasury), salt);
        vm.deal(forwarder, 1 ether);
        hookTreasury.setCallback(
            address(factory),
            abi.encodeCall(
                ForwarderFactory.flush, (address(hookTreasury), _single(salt), address(0))
            )
        );

        factory.flush(address(hookTreasury), _single(salt), address(0));

        assertTrue(hookTreasury.hookAttempted());
        assertTrue(hookTreasury.hookRejected());
        assertEq(address(hookTreasury).balance, 1 ether);
    }

    // Fuzz

    function testFuzz_PredictionMatchesDeployment(address owner, bytes32 salt, uint96 rawAmount)
        public
    {
        vm.assume(owner != address(0));
        uint256 amount = bound(uint256(rawAmount), 1, type(uint96).max);
        address predicted = factory.addressOf(owner, salt);
        token.mint(predicted, amount);

        factory.flush(owner, _single(salt), address(token));

        assertGt(predicted.code.length, 0);
        assertEq(Forwarder(payable(predicted)).treasury(), owner);
        assertEq(token.balanceOf(predicted), 0);
        assertEq(token.balanceOf(owner), amount);
    }

    function testFuzz_FlushesFullEthBalance(bytes32 salt, uint96 rawAmount) public {
        uint256 amount = bound(uint256(rawAmount), 1, type(uint96).max);
        address forwarder = factory.addressOf(treasury, salt);
        vm.deal(forwarder, amount);

        _flush(salt, address(0));

        assertEq(forwarder.balance, 0);
        assertEq(treasury.balance, amount);
    }

    function _flush(bytes32 salt, address asset) private {
        _flushBatch(_single(salt), asset);
    }

    function _flushBatch(bytes32[] memory salts, address asset) private {
        vm.prank(caller);
        factory.flush(treasury, salts, asset);
    }

    function _single(bytes32 salt) private pure returns (bytes32[] memory salts) {
        salts = new bytes32[](1);
        salts[0] = salt;
    }

    function _pair(bytes32 first, bytes32 second) private pure returns (bytes32[] memory salts) {
        salts = new bytes32[](2);
        salts[0] = first;
        salts[1] = second;
    }
}
