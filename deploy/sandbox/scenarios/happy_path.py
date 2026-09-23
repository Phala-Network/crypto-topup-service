"""Quote-first payment of the exact locked amount, then a persistent-address payment.

Expect: before finality, the lock shows the payment as `seen` (in time, within tolerance), a
provisional `deposit.pending` webhook arrives, and the persistent payment is listed as a pending
deposit; neither is credited yet. Then the lock deposit is credited at the lock price with exactly
the quoted credit and the lock becomes `consumed`; the persistent deposit is credited at spot;
both produce verified `deposit.confirmed` and `deposit.credited` webhooks and one product ledger
credit each.
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, check
from topup_client.models import RateLockPayment

SEEN_TIMEOUT_S = 60.0


def run(ctx: Context) -> None:
    team, persistent = ctx.team("happy")
    lock_ref, lock = ctx.lock(team, amount_minor=2500)
    tx_hash = ctx.pay(lock.address, int(lock.amount_atomic))
    payment = seen_lock_payment(ctx, team, lock_ref)
    check(payment.tx_hash == tx_hash, "seen payment is not the sent transaction")
    check(payment.supported and payment.in_time, "seen payment is not in time for the lock asset")
    check(payment.amount_within_tolerance, "exact payment is outside the lock tolerance")
    pending = ctx.event("deposit.pending", lambda data: data.get("tx_hash") == tx_hash)
    check(pending["provisional"] is True, "deposit.pending is not marked provisional")
    check(pending["product_lock_ref"] == lock_ref, "deposit.pending does not name the lock")
    deposit, confirmed = ctx.credited(team, lock.address, lock)
    check(confirmed["price_source"] == "lock", "lock payment was not valued at the lock price")
    check(deposit.credit_minor == lock.credit_minor, "credit differs from the quoted credit")
    check(deposit.price_scaled == lock.price_scaled, "price differs from the locked price")
    check(deposit.lock_ref == lock_ref, "deposit does not reference its lock")
    check(ctx.client.get_rate_lock(team, lock_ref).status == "consumed", "lock not consumed")

    tx_hash = ctx.pay(persistent, 1000 * TOKEN_UNIT)
    ctx.wait_until(
        lambda: any(item.tx_hash == tx_hash for item in ctx.client.list_pending_deposits(team)),
        "persistent payment was never listed as pending before finality",
        SEEN_TIMEOUT_S,
    )
    _, confirmed = ctx.credited(team, persistent)
    check(confirmed["price_source"] == "spot", "persistent payment was not valued at spot")


def seen_lock_payment(ctx: Context, team: str, lock_ref: str) -> RateLockPayment:
    """Polls the lock until its payment is `seen`; failing if it is final or credited first."""
    found: list[RateLockPayment] = []

    def seen() -> bool:
        lock = ctx.client.get_rate_lock(team, lock_ref)
        check(lock.status == "open", f"lock is {lock.status} before its payment was shown as seen")
        if isinstance(lock.payment, RateLockPayment):
            check(lock.payment.status == "seen", f"payment is {lock.payment.status}, not seen")
            found.append(lock.payment)
        return bool(found)

    ctx.wait_until(seen, "lock payment was never shown as seen", SEEN_TIMEOUT_S)
    return found[0]
