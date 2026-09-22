"""Quote-first payment of the exact locked amount, then a persistent-address payment.

Expect: the lock deposit is credited at the lock price with exactly the quoted credit and the
lock becomes `consumed`; the persistent deposit is credited at spot; both produce verified
`deposit.confirmed` and `deposit.credited` webhooks and one product ledger credit each.
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, check


def run(ctx: Context) -> None:
    team, persistent = ctx.team("happy")
    lock_ref, lock = ctx.lock(team, amount_minor=2500)
    ctx.pay(lock.address, int(lock.amount_atomic))
    deposit, confirmed = ctx.credited(team, lock.address, lock)
    check(confirmed["price_source"] == "lock", "lock payment was not valued at the lock price")
    check(deposit.credit_minor == lock.credit_minor, "credit differs from the quoted credit")
    check(deposit.price_scaled == lock.price_scaled, "price differs from the locked price")
    check(deposit.lock_ref == lock_ref, "deposit does not reference its lock")
    check(ctx.client.get_rate_lock(team, lock_ref).status == "consumed", "lock not consumed")

    ctx.pay(persistent, 1000 * TOKEN_UNIT)
    _, confirmed = ctx.credited(team, persistent)
    check(confirmed["price_source"] == "spot", "persistent payment was not valued at spot")
