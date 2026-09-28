"""3% less than the locked amount, outside the 1% lock tolerance.

Expect: the deposit is credited at spot for what arrived, below the quoted credit, and the lock
is not completed. Once the address has received funds the quote can no longer be canceled:
`400 quote_payment_received` while the payment window is open, or `400` once the window has
closed (on Sepolia finality outlasts the quote window).
"""

from __future__ import annotations

import time

from harness import Context, check, credit
from topup_sdk import ApiError

ALL_STATES = {"pending", "credited", "swept", "rejected"}


def run(ctx: Context) -> None:
    team = ctx.team("under")
    lock_ref, lock = ctx.lock(team, amount_minor=2500)
    ctx.pay(lock.address, int(lock.amount_atomic) * 97 // 100)
    ctx.deposit(team, lock.address, ALL_STATES)
    current = ctx.client.get_quote(lock_ref)
    still_open = current.status == "open" and current.expires_at - time.time() >= 5
    try:
        ctx.client.cancel_quote(lock_ref)
    except ApiError as error:
        expected = "quote_payment_received" if still_open else error.code
        check(
            error.status_code == 400 and error.code == expected,
            f"cancel failed with {error.status_code} {error.code}",
        )
    else:
        check(False, "a paid lock was cancelled")

    deposit, credited = ctx.credited(team, lock.address, lock)
    check(credited["price_source"] == "spot", "underpayment was valued at the lock price")
    check(credit(deposit) < lock.amount, "underpayment was credited in full")
    check(ctx.client.get_quote(lock_ref).status != "complete", "lock was completed")
