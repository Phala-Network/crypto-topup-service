"""Shared harness for sandbox scenarios.

Scenarios run the reference product from sdk/examples/phala_cloud_integration.py as the
integrator's settlement and webhook endpoint, drive payments with the sandbox test token, and
assert the deposit states and verified webhooks the service produces. Each scenario uses fresh
workspaces, so scenarios are independent and can run against a shared sandbox.
"""

from __future__ import annotations

import json
import logging
import subprocess
import sys
import threading
import time
import uuid
from collections import Counter
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

REPOSITORY = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPOSITORY / "sdk/examples"))

import phala_cloud_integration as reference  # noqa: E402
from topup_client.models import DepositResponse, RateLockResponse  # noqa: E402
from topup_sdk import TopupClient  # noqa: E402
from topup_sdk.addresses import same_address  # noqa: E402

LOG = logging.getLogger("sandbox")
TOKEN_UNIT = 10**18
FINAL_STATES = {"credited", "swept", "rejected"}
DEPOSIT_TIMEOUT_S = 600.0
EVENT_TIMEOUT_S = 180.0


class ScenarioFailure(AssertionError):
    """An expected state or event did not occur."""


class ScenarioSkipped(Exception):
    """The scenario cannot run in this environment."""


def check(condition: bool, message: str) -> None:
    if not condition:
        raise ScenarioFailure(message)


def credit(deposit: DepositResponse) -> int:
    """The deposit's credit in minor units; fails if the service has not valued it."""
    check(isinstance(deposit.credit_minor, str), f"deposit {deposit.id} has no credit")
    return int(str(deposit.credit_minor))


class ScenarioSettlement(reference.SettlementService):
    """Reference settlement endpoint with request counters and one injectable fault."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self.posts: Counter[str] = Counter()
        self.gets: Counter[str] = Counter()
        self.lose_answer_once: set[str] = set()
        self._lock = threading.Lock()

    def handle_post(self, target: str, headers: Mapping[str, str], body: bytes) -> reference.Answer:
        answer = super().handle_post(target, headers, body)
        try:
            team = str(json.loads(body)["account_id"])
        except (ValueError, KeyError, TypeError):
            return answer
        with self._lock:
            self.posts[team] += 1
            lose = answer.status == 200 and team in self.lose_answer_once
            self.lose_answer_once.discard(team)
        if lose:
            # The credit is committed; the answer is lost on the way back to the service.
            LOG.info("dropping the committed settlement answer for %s", team)
            return reference.Answer(500)
        return answer

    def handle_get(self, target: str, headers: Mapping[str, str]) -> reference.Answer:
        answer = super().handle_get(target, headers)
        order = self.ledger.find_order(target.rsplit("/", 1)[-1])
        if order is not None and order.team_id is not None:
            with self._lock:
                self.gets[order.team_id] += 1
        return answer


@dataclass
class Context:
    config: reference.SandboxConfig
    client: TopupClient
    ledger: reference.ProductLedger
    settlement: ScenarioSettlement
    payer: reference.Payer
    run_id: str = field(default_factory=lambda: uuid.uuid4().hex[:8])

    def team(self, name: str, *, suspended: bool = False) -> tuple[str, str]:
        """Registers a fresh workspace and returns `(team_id, persistent_address)`."""
        team = f"{name}-{self.run_id}"
        address = reference.register_team(
            self.config, self.client, self.ledger, team, suspended=suspended
        )
        return team, address

    def lock(self, team: str, amount_minor: int) -> tuple[str, RateLockResponse]:
        lock_ref = f"lock-{uuid.uuid4().hex[:12]}"
        lock = reference.create_quote(
            self.config,
            self.client,
            self.ledger,
            team,
            lock_ref=lock_ref,
            amount_minor=amount_minor,
        )
        return lock_ref, lock

    def pay(self, to: str, amount_atomic: int, token: str | None = None) -> str:
        tx_hash = self.payer.mint_and_transfer(token or self.config.token, to, amount_atomic)
        LOG.info("sent %s atomic to %s in %s", amount_atomic, to, tx_hash)
        return tx_hash

    def deposit(self, team: str, address: str, states: set[str] = FINAL_STATES) -> DepositResponse:
        deposit: DepositResponse = reference.wait_for_deposit(
            self.client, team, address, states, DEPOSIT_TIMEOUT_S
        )
        return deposit

    def event(self, event_type: str, matches: Callable[[dict[str, Any]], bool]) -> dict[str, Any]:
        return self.ledger.wait_for_event(event_type, matches, EVENT_TIMEOUT_S)

    def deposit_event(self, event_type: str, deposit: DepositResponse) -> dict[str, Any]:
        return self.event(event_type, lambda data: data.get("deposit_id") == str(deposit.id))

    def credited(
        self, team: str, address: str, lock: RateLockResponse | None = None
    ) -> tuple[DepositResponse, dict[str, Any]]:
        """Waits for credit and checks the webhook and product ledger agree with the service."""
        deposit = self.deposit(team, address)
        check(deposit.state in {"credited", "swept"}, f"deposit is {deposit.state}, not credited")
        confirmed = self.deposit_event("deposit.confirmed", deposit)
        credited = self.deposit_event("deposit.credited", deposit)
        check(
            credited["amount_minor"] == deposit.credit_minor,
            "deposit.credited amount differs from the deposit's credit",
        )
        credits = [
            amount
            for key, amount in self.ledger.credits_for(team)
            if key == f"deposit:{deposit.id}"
        ]
        check(credits == [credit(deposit)], f"product ledger holds {credits}")
        if lock is not None:
            check(same_address(deposit.address, lock.address), "deposit is not at the lock")
        return deposit, confirmed

    def restart_service(self) -> None:
        if not self.config.restart_command:
            raise ScenarioSkipped("no restart_command configured (operator-only on Sepolia)")
        LOG.info("restarting the service: %s", " ".join(self.config.restart_command))
        subprocess.run(self.config.restart_command, check=True, capture_output=True)

    def wait_until(self, condition: Callable[[], bool], message: str, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while not condition():
            if time.monotonic() > deadline:
                raise ScenarioFailure(message)
            time.sleep(1)
