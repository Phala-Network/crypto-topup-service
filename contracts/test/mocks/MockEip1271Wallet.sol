// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { ECDSA } from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";

/// @dev An EIP-1271 contract wallet with the two ways a Safe approves a message: an owner's
/// signature (a Safe verifies its owners' signatures of the SafeMessage; this mock takes the
/// owner's signature of the hash itself), and an on-chain approval with an empty signature, as
/// Safe's `SignMessageLib` records in `signedMessages`.
contract MockEip1271Wallet {
    bytes4 internal constant MAGIC_VALUE = 0x1626ba7e;

    address public immutable owner;
    mapping(bytes32 hash => bool approved) public signedMessages;

    error NotOwner();

    constructor(address owner_) {
        owner = owner_;
    }

    function signMessage(bytes32 hash) external {
        if (msg.sender != owner) revert NotOwner();
        signedMessages[hash] = true;
    }

    function isValidSignature(bytes32 hash, bytes calldata signature)
        external
        view
        returns (bytes4)
    {
        if (signature.length == 0) {
            return signedMessages[hash] ? MAGIC_VALUE : bytes4(0xffffffff);
        }
        (address signer, ECDSA.RecoverError recoverError,) = ECDSA.tryRecover(hash, signature);
        return recoverError == ECDSA.RecoverError.NoError && signer == owner
            ? MAGIC_VALUE
            : bytes4(0xffffffff);
    }
}
