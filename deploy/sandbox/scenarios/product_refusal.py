"""The product refuses the credit, here because the workspace is suspended.

Expect: the product durably records `rejected` without a ledger credit; the service moves the
deposit to `rejected` and emits `deposit.rejected` with reason `product_refused` and the
product's own reason.
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, check


def run(ctx: Context) -> None:
    team, persistent = ctx.team("refused", suspended=True)
    ctx.pay(persistent, 1000 * TOKEN_UNIT)
    deposit = ctx.deposit(team, persistent)
    check(deposit.state == "rejected", f"deposit is {deposit.state}, not rejected")
    rejected = ctx.deposit_event("deposit.rejected", deposit)
    check(rejected["reason"] == "product_refused", f"reason is {rejected['reason']}")
    check(rejected["product_reason"] == "account_suspended", "product reason was not kept")
    order = ctx.ledger.find_order(f"deposit:{deposit.id}")
    check(order is not None and order.status == "rejected", "refusal was not recorded")
    check(ctx.ledger.credits_for(team) == [], "a refused deposit was credited")
