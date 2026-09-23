"""3% less than the locked amount, outside the 1% lock tolerance.

Expect: the deposit is credited at spot for what arrived, below the quoted credit, and the lock
is not consumed. Once the address has received funds the lock can no longer be cancelled:
`409 pending_payment` while the payment window is open, or `409` once the window has closed (on
Sepolia finality outlasts the lock window).
"""

from __future__ import annotations

from harness import Context, check, credit
from topup_sdk import ApiError

ALL_STATES = {"detected", "confirmed", "cleared", "credited", "swept", "rejected"}


def run(ctx: Context) -> None:
    team, _ = ctx.team("under")
    lock_ref, lock = ctx.lock(team, amount_minor=2500)
    ctx.pay(lock.address, int(lock.amount_atomic) * 97 // 100)
    ctx.deposit(team, lock.address, ALL_STATES)
    current = ctx.client.get_rate_lock(team, lock_ref)
    still_open = current.status == "open" and current.remaining_seconds >= 5
    try:
        ctx.client.cancel_rate_lock(team, lock_ref)
    except ApiError as error:
        expected = "pending_payment" if still_open else error.code
        check(
            error.status_code == 409 and error.code == expected,
            f"cancel failed with {error.status_code} {error.code}",
        )
    else:
        check(False, "a paid lock was cancelled")

    deposit, confirmed = ctx.credited(team, lock.address, lock)
    check(confirmed["price_source"] == "spot", "underpayment was valued at the lock price")
    check(credit(deposit) < int(lock.credit_minor), "underpayment was credited in full")
    check(ctx.client.get_rate_lock(team, lock_ref).status != "consumed", "lock was consumed")
