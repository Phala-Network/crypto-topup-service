"""A token with no route is sent to a persistent address.

Expect: the deposit is `rejected` with `deposit.rejected` reason `unsupported_asset`, and the
product never receives `deposit.credited` for it.
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, ScenarioSkipped, check, deposit_uuid


def run(ctx: Context) -> None:
    if ctx.config.unsupported_token is None:
        raise ScenarioSkipped("no unsupported_token configured")
    team, persistent = ctx.team("asset")
    ctx.pay(persistent, 1000 * TOKEN_UNIT, token=ctx.config.unsupported_token)
    deposit = ctx.deposit(team, persistent)
    check(deposit.status == "rejected", f"deposit is {deposit.status}, not rejected")
    rejected = ctx.deposit_event("deposit.rejected", deposit)
    check(rejected["reason"] == "unsupported_asset", f"reason is {rejected['reason']}")
    check(
        ctx.ledger.find_order(f"deposit:{deposit_uuid(deposit)}") is None,
        "product recorded an order",
    )
    check(ctx.fulfillment.deliveries[team] == 0, "product received deposit.credited")
