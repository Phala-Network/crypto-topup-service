"""Payment of a quote's exact amount, then a second payment to the same address.

Expect: before finality, the quote shows the payment as `seen` and matching, and a provisional
`deposit.pending` webhook arrives; nothing is credited yet. Then the deposit is credited at the
quoted price with exactly the quoted credit and the quote becomes `complete`. The second payment,
to the completed quote's address, is credited at spot. Both produce verified `deposit.confirmed`
and `deposit.credited` webhooks and one product ledger credit each.
"""

from __future__ import annotations

from harness import TOKEN_UNIT, Context, check
from topup_client.models import QuotePayment

SEEN_TIMEOUT_S = 60.0


def run(ctx: Context) -> None:
    team = ctx.team("happy")
    lock_ref, lock = ctx.lock(team, amount_minor=2500)
    tx_hash = ctx.pay(lock.address, int(lock.amount_atomic))
    payment = seen_lock_payment(ctx, team, lock_ref)
    check(payment.tx_hash == tx_hash, "seen payment is not the sent transaction")
    check(payment.matches_quote, "the exact, in-time payment does not match its quote")
    pending = ctx.event("deposit.pending", lambda data: data.get("tx_hash") == tx_hash)
    check(pending["provisional"] is True, "deposit.pending is not marked provisional")
    check(pending["product_lock_ref"] == lock_ref, "deposit.pending does not name the lock")
    deposit, confirmed = ctx.credited(team, lock.address, lock)
    check(confirmed["price_source"] == "lock", "lock payment was not valued at the lock price")
    check(deposit.amount == lock.amount, "credit differs from the quoted credit")
    check(
        deposit.exchange_rate == lock.exchange_rate,
        "price differs from the locked price",
    )
    check(deposit.quote == lock_ref, "deposit does not reference its quote")
    check(ctx.client.get_quote(lock_ref).status == "complete", "lock not completed")

    tx_hash = ctx.pay(lock.address, 1000 * TOKEN_UNIT)
    second, confirmed = ctx.credited(team, lock.address, tx_hash=tx_hash)
    check(confirmed["price_source"] == "spot", "a second payment was not valued at spot")
    check(second.quote == lock_ref, "the second deposit does not name its quote")


def seen_lock_payment(ctx: Context, team: str, lock_ref: str) -> QuotePayment:
    """Polls the lock until its payment is `seen`; failing if it is final or credited first."""
    found: list[QuotePayment] = []

    def seen() -> bool:
        lock = ctx.client.get_quote(lock_ref)
        check(lock.status == "open", f"lock is {lock.status} before its payment was shown as seen")
        if isinstance(lock.payment, QuotePayment):
            check(lock.payment.status == "seen", f"payment is {lock.payment.status}, not seen")
            found.append(lock.payment)
        return bool(found)

    ctx.wait_until(seen, "lock payment was never shown as seen", SEEN_TIMEOUT_S)
    return found[0]
