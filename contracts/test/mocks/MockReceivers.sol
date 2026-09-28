// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// A treasury contract that refuses ETH.
contract RejectingTreasury {
    receive() external payable {
        revert();
    }
}

/// A treasury contract that burns all gas it is given.
contract GasGriefingTreasury {
    uint256 public counter;

    receive() external payable {
        while (true) {
            ++counter;
        }
    }
}

/// Receives ETH like a Safe proxy: delegates to a singleton that writes storage and emits an event.
contract EventReceiverSingleton {
    event SafeReceived(address indexed sender, uint256 value);

    uint256 public received;

    receive() external payable {
        received += msg.value;
        emit SafeReceived(msg.sender, msg.value);
    }
}

contract DelegatingReceiver {
    address internal immutable singleton;
    uint256 public received;

    constructor(address singleton_) {
        singleton = singleton_;
    }

    receive() external payable {
        address target = singleton;
        assembly ("memory-safe") {
            let success := delegatecall(gas(), target, 0, 0, 0, 0)
            if iszero(success) { revert(0, 0) }
        }
    }
}

/// A treasury that calls back into a configured target (the factory) when it receives ETH.
contract ReentrantTreasury {
    address public target;
    bytes public callback;
    bool public hookAttempted;
    bool public hookRejected;

    function setCallback(address target_, bytes calldata callback_) external {
        target = target_;
        callback = callback_;
    }

    receive() external payable {
        if (target != address(0) && !hookAttempted) {
            hookAttempted = true;
            (bool success,) = target.call(callback);
            hookRejected = !success;
        }
    }
}

/// A treasury whose token-received hook burns all its gas for transfers from the forwarders it
/// is told to, and accepts every other transfer.
contract ExpensiveHookTreasury {
    mapping(address from => bool) public expensive;
    uint256 public counter;

    function setExpensive(address from) external {
        expensive[from] = true;
    }

    function onTokenReceived(address from, uint256) external {
        if (!expensive[from]) return;
        while (true) {
            ++counter;
        }
    }
}
