// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { Test } from "forge-std/Test.sol";

import { MockSanctionsOracle } from "./mocks/MockSanctionsOracle.sol";

contract MockSanctionsOracleTest is Test {
    MockSanctionsOracle private oracle;

    function setUp() public {
        oracle = new MockSanctionsOracle();
    }

    function test_DefaultsToClearAndStoresSanctionedAddresses() public {
        address clear = makeAddr("clear");
        address sanctioned = makeAddr("sanctioned");

        oracle.setSanctioned(sanctioned, true);

        assertFalse(oracle.isSanctioned(clear));
        assertTrue(oracle.isSanctioned(sanctioned));
    }

    function test_CanClearAnAddressAgain() public {
        address account = makeAddr("account");
        oracle.setSanctioned(account, true);
        oracle.setSanctioned(account, false);

        assertFalse(oracle.isSanctioned(account));
    }
}
