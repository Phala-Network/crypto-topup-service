"""Overpayment within and beyond the 1% lock tolerance.

Expect: +0.5% consumes the lock and is credited exactly the quoted credit at the lock price;
+5% leaves its lock unconsumed and is credited at spot for the full amount received.
"""

from __future__ import annotations

from harness import Context, check, credit


def run(ctx: Context) -> None:
    team = ctx.team("over")
    within_ref, within = ctx.lock(team, amount_minor=2500)
    ctx.pay(within.address, int(within.amount_atomic) * 1005 // 1000)
    deposit, credited = ctx.credited(team, within.address, within)
    check(credited["price_source"] == "quote", "in-tolerance payment was not valued at the lock")
    check(deposit.amount == within.amount, "in-tolerance credit differs from quote")
    check(ctx.client.get_quote(within_ref).status == "complete", "lock not completed")

    beyond_ref, beyond = ctx.lock(team, amount_minor=2500)
    ctx.pay(beyond.address, int(beyond.amount_atomic) * 105 // 100)
    deposit, credited = ctx.credited(team, beyond.address, beyond)
    check(credited["price_source"] == "spot", "overpayment was valued at the lock price")
    check(credit(deposit) > beyond.amount, "overpayment was capped")
    check(ctx.client.get_quote(beyond_ref).status != "complete", "lock was completed")
