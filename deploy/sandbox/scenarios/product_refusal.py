"""The product refuses the credit, here because the workspace is suspended.

Expect: the service credits the deposit and emits `deposit.credited`; the product holds it
without a ledger credit (`account_suspended`) and requests its refund, which stays `pending` until
the merchant pays it and attaches the transaction with `mark_paid`.
"""

from __future__ import annotations

from harness import Context, check

# A refund destination the user would supply; never the deposit's sender.
REFUND_TO = "0x" + "11" * 20


def run(ctx: Context) -> None:
    team = ctx.team("refused", suspended=True)
    _, quote = ctx.lock(team, amount_minor=2500)
    ctx.pay(quote.address, int(quote.amount_atomic))
    deposit = ctx.deposit(team, quote.address)
    check(deposit.status == "credited", f"deposit is {deposit.status}, not credited")
    ctx.deposit_event("deposit.credited", deposit)
    order = ctx.ledger.find_order(deposit.id)
    check(order is not None and order.status == "held", "the refusal was not recorded")
    check(order is not None and order.reason == "account_suspended", "wrong hold reason")
    check(ctx.ledger.credits_for(team) == [], "a refused deposit was credited")
    refund = ctx.client.create_refund(deposit.id, REFUND_TO, int(deposit.amount_atomic))
    check(refund.status == "pending", f"refund is {refund.status}, not pending")
