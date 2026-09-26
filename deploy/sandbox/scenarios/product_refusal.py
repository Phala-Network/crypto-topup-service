"""The product refuses the credit, here because the workspace is suspended.

Expect: the service credits the deposit and emits `deposit.credited`; the product holds it
without a ledger credit (`account_suspended`) and requests its refund, which waits for finance
(deploy/runbooks/refund-execution.md).
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, check

# A refund destination the user would supply; never the deposit's sender.
REFUND_TO = "0x" + "11" * 20


def run(ctx: Context) -> None:
    team, persistent = ctx.team("refused", suspended=True)
    ctx.pay(persistent, 1000 * TOKEN_UNIT)
    deposit = ctx.deposit(team, persistent)
    check(deposit.state in {"credited", "swept"}, f"deposit is {deposit.state}, not credited")
    ctx.deposit_event("deposit.credited", deposit)
    order = ctx.ledger.find_order(f"deposit:{deposit.id}")
    check(order is not None and order.status == "held", "the refusal was not recorded")
    check(order is not None and order.reason == "account_suspended", "wrong hold reason")
    check(ctx.ledger.credits_for(team) == [], "a refused deposit was credited")
    refund = ctx.client.request_refund(deposit.id, REFUND_TO, int(deposit.amount_atomic))
    check(refund.status == "requested", f"refund is {refund.status}, not requested")
