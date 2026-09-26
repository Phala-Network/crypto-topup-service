"""The service restarts before the product's acknowledgement of a credit arrives.

The product fulfills `deposit.credited` but answers 500 until the service has restarted. Expect:
the restarted service delivers the same event again, the product answers 2xx without a second
credit, and the ledger holds exactly one credit.
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, check


def run(ctx: Context) -> None:
    if not ctx.config.restart_command:
        ctx.restart_service()  # raises ScenarioSkipped with the reason
    team, persistent = ctx.team("restart")
    ctx.fulfillment.lose_acks.add(team)
    ctx.pay(persistent, 1000 * TOKEN_UNIT)
    ctx.wait_until(
        lambda: ctx.fulfillment.deliveries[team] >= 1,
        "the service never delivered deposit.credited",
        timeout=600,
    )
    ctx.restart_service()
    ctx.fulfillment.lose_acks.discard(team)
    ctx.wait_until(
        lambda: ctx.fulfillment.deliveries[team] >= 2,
        "the service did not deliver deposit.credited again",
        timeout=600,
    )
    ctx.credited(team, persistent)
    check(len(ctx.ledger.credits_for(team)) == 1, "the deposit was credited more than once")
