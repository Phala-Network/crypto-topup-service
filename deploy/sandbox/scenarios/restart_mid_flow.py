"""The service restarts while a settlement answer is unknown.

The product commits the credit but the answer is lost (HTTP 500), then the service restarts.
Until the restart completes, the product answers the team's `GET` by key with 503, so the
lookup cannot slip in before the restart. Expect: after restart the service asks `GET` by key
instead of resending, adopts the committed answer, and the deposit is credited exactly once.
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, check


def run(ctx: Context) -> None:
    if not ctx.config.restart_command:
        ctx.restart_service()  # raises ScenarioSkipped with the reason
    team, persistent = ctx.team("restart")
    ctx.settlement.lose_answer_once.add(team)
    ctx.settlement.hold_lookups.add(team)
    ctx.pay(persistent, 1000 * TOKEN_UNIT)
    ctx.wait_until(
        lambda: ctx.settlement.posts[team] >= 1,
        "the service never sent the settlement request",
        timeout=600,
    )
    ctx.restart_service()
    ctx.settlement.hold_lookups.discard(team)
    ctx.credited(team, persistent)
    check(ctx.settlement.gets[team] >= 1, "the service did not look up the key after restart")
    check(ctx.settlement.posts[team] == 1, "the service resent instead of adopting the answer")
    check(len(ctx.ledger.credits_for(team)) == 1, "the deposit was credited more than once")
