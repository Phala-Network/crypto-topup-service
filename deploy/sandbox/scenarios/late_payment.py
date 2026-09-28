"""The exact locked amount arrives after the lock expired.

Expect: `quote.expired` is delivered first; the late deposit is still credited, valued at
spot rather than the lock price, and the lock stays `expired`.
"""

from __future__ import annotations

from harness import Context, check


def run(ctx: Context) -> None:
    team = ctx.team("late")
    lock_ref, lock = ctx.lock(team, amount_minor=2500)
    expired = ctx.event("quote.expired", lambda quote: quote["id"] == lock_ref)
    check(
        expired["client_reference_id"] == team and expired["amount"] == lock.amount,
        "expiry event names another quote",
    )
    check(expired["status"] == "expired", f"expired quote is {expired['status']}")
    ctx.pay(lock.address, int(lock.amount_atomic))
    deposit, credited = ctx.credited(team, lock.address, lock)
    check(credited["price_source"] == "spot", "late payment was valued at the lock price")
    check(ctx.client.get_quote(lock_ref).status == "expired", "lock is not expired")
    check(deposit.quote == lock_ref, "deposit does not reference its quote")
