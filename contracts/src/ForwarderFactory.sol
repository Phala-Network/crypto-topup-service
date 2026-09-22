// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { AccessControl } from "@openzeppelin/contracts/access/AccessControl.sol";
import { Clones } from "@openzeppelin/contracts/proxy/Clones.sol";

import { Forwarder } from "./Forwarder.sol";

contract ForwarderFactory is AccessControl {
    error ForwarderFlushFailed(bytes32 salt, address forwarder, bytes reason);
    error ZeroAdmin();

    bytes32 public constant OPERATOR_ROLE = keccak256("OPERATOR_ROLE");

    Forwarder public immutable implementation;

    event Flushed(
        bytes32 indexed salt, address indexed forwarder, address indexed token, uint256 amount
    );

    constructor(address admin, address treasury) {
        if (admin == address(0)) revert ZeroAdmin();

        implementation = new Forwarder(treasury, address(this));
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
    }

    function addressOf(bytes32 salt) public view returns (address) {
        return Clones.predictDeterministicAddress(address(implementation), salt, address(this));
    }

    function flush(bytes32[] calldata salts, address token) external onlyRole(OPERATOR_ROLE) {
        uint256 length = salts.length;
        for (uint256 i; i < length; ++i) {
            bytes32 salt = salts[i];
            address forwarder = addressOf(salt);
            if (forwarder.code.length == 0) {
                forwarder = Clones.cloneDeterministic(address(implementation), salt);
            }

            // The batch intentionally performs one external flush per salt.
            // forge-lint: disable-next-line(calls-loop)
            try Forwarder(payable(forwarder)).flush(token) returns (uint256 amount) {
                // The event must contain the amount returned by the completed transfer.
                // forge-lint: disable-next-line(reentrancy-events)
                emit Flushed(salt, forwarder, token, amount);
            } catch (bytes memory reason) {
                // Atomic rollback is the documented batch failure policy.
                // forge-lint: disable-next-line(require-revert-in-loop)
                revert ForwarderFlushFailed(salt, forwarder, reason);
            }
        }
    }
}
