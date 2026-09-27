"""Payment of a quote's exact amount, then a second payment to the same address.

Expect: before finality, the quote shows the payment as `seen` and matching, and the payer's view
by `client_secret` shows it too; nothing is credited yet. Then the deposit is credited at the
quoted price with exactly the quoted credit and the quote becomes `complete`. The second payment,
to the completed quote's address, is credited at spot. Both produce a verified `deposit.credited`
webhook and one product ledger credit each.
"""

from __future__ import annotations

import httpx

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
    check_client_view(ctx, lock_ref, str(lock.client_secret))
    deposit, credited = ctx.credited(team, lock.address, lock)
    check(credited["price_source"] == "quote", "lock payment was not valued at the lock price")
    check(deposit.amount == lock.amount, "credit differs from the quoted credit")
    check(
        deposit.exchange_rate == lock.exchange_rate,
        "price differs from the locked price",
    )
    check(deposit.quote == lock_ref, "deposit does not reference its quote")
    check(ctx.client.get_quote(lock_ref).status == "complete", "lock not completed")

    tx_hash = ctx.pay(lock.address, 1000 * TOKEN_UNIT)
    second, credited = ctx.credited(team, lock.address, tx_hash=tx_hash)
    check(credited["price_source"] == "spot", "a second payment was not valued at spot")
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


def check_client_view(ctx: Context, quote_id: str, client_secret: str) -> None:
    """The payer's page reads the quote by its client secret alone, from any origin."""
    response = httpx.get(
        f"{ctx.config.service_url}/v1/quotes/{quote_id}",
        params={"client_secret": client_secret},
        timeout=20,
    )
    check(response.status_code == 200, f"client read answered {response.status_code}")
    check(response.headers.get("access-control-allow-origin") == "*", "client read lacks CORS")
    view = response.json()
    check("account_id" not in view, "the payer's view names the account")
    check(
        view["payment_status"] in {"seen", "confirming", "credited"},
        f"payer's view shows {view['payment_status']}",
    )
