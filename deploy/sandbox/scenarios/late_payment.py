"""The exact locked amount arrives after the lock expired.

Expect: `rate_lock.expired` is delivered first; the late deposit is still credited, valued at
spot rather than the lock price, and the lock stays `expired`.
"""

from __future__ import annotations

from harness import Context, check


def run(ctx: Context) -> None:
    team, _ = ctx.team("late")
    lock_ref, lock = ctx.lock(team, amount_minor=2500)
    expired = ctx.event(
        "rate_lock.expired",
        lambda data: data.get("external_id") == team and data.get("product_lock_ref") == lock_ref,
    )
    check(expired["credit_minor"] == str(lock.amount), "expiry event names another quote")
    ctx.pay(lock.address, int(lock.amount_atomic))
    deposit, confirmed = ctx.credited(team, lock.address, lock)
    check(confirmed["price_source"] == "spot", "late payment was valued at the lock price")
    check(ctx.client.get_quote(lock_ref).status == "expired", "lock is not expired")
    check(deposit.quote == lock_ref, "deposit does not reference its quote")
