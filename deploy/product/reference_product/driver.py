"""The deposit driver: plays a Phala Cloud user against a running product.

It registers a workspace through the product's account API, gets a quote and recomputes its
address locally, pays the exact locked amount with the test token, polls until the deposit is
credited, and checks that the product ledger credited the locked amount exactly once and received
the verified `deposit.credited` webhook. Its options drive the abnormal paths instead: a different
amount, a payment after the quote window, a payment to the persistent address, another token, and
a refund request for a rejected deposit (deploy/README.md, "Abnormal paths").
"""

from __future__ import annotations

import getpass
import logging
import os
import time
import uuid
from collections.abc import Callable
from http import HTTPStatus
from pathlib import Path
from typing import Any
from urllib.parse import quote

import httpx
from eth_account import Account
from web3 import Web3
from web3.contract.contract import ContractFunction
from web3.middleware import SignAndSendRawMiddlewareBuilder

from topup_client.models import DepositResponse, RateLockResponse
from topup_sdk import RequestSigner, SigningAuth
from topup_sdk.addresses import same_address

from .config import ProductConfig
from .server import quote_address

LOG = logging.getLogger(__name__)

# The driver pays this long after a quote's `expires_at` to be late by chain time too.
LATE_MARGIN_S = 60
# The test token's `mint` and `transfer`, both `(address to, uint256 amount)`.
TOKEN_ABI = [
    {
        "type": "function",
        "name": name,
        "inputs": [{"name": "to", "type": "address"}, {"name": "amount", "type": "uint256"}],
        "outputs": [],
        "stateMutability": "nonpayable",
    }
    for name in ("mint", "transfer")
]


class Payer:
    """Sends test-token transactions with web3.py.

    On Anvil the payer is `payer`, an unlocked development account. On Sepolia it signs with a
    Foundry keystore holding a funded throwaway test key: the account named by `payer_account`
    (`cast wallet import`), or else the keystore file named by `ETH_KEYSTORE`. As with `cast`, the
    keystore password comes from the mode-0600 file named by `ETH_PASSWORD`, or is prompted for;
    no key is ever passed in the environment or on a command line. The test token's `mint` is
    public, so the payer mints what it pays and needs only Sepolia ETH for gas.
    """

    def __init__(self, config: ProductConfig) -> None:
        self._web3 = Web3(Web3.HTTPProvider(config.rpc_url))
        keystore = os.environ.get("ETH_KEYSTORE")
        if config.payer_account is not None:
            keystore = str(Path.home() / ".foundry/keystores" / config.payer_account)
        if keystore:
            account = Account.from_key(Account.decrypt(Path(keystore).read_text(), _password()))
            signer = SignAndSendRawMiddlewareBuilder.build(account)
            self._web3.middleware_onion.inject(signer, layer=0)
            self.address = account.address
        elif config.payer is not None:
            self.address = Web3.to_checksum_address(config.payer)
        else:
            raise ValueError("set payer (Anvil), payer_account, or ETH_KEYSTORE")

    def mint_and_transfer(self, token: str, to: str, amount_atomic: int) -> str:
        erc20 = self._web3.eth.contract(Web3.to_checksum_address(token), abi=TOKEN_ABI)
        self._send(erc20.functions.mint(self.address, amount_atomic))
        return self._send(erc20.functions.transfer(Web3.to_checksum_address(to), amount_atomic))

    def _send(self, call: ContractFunction) -> str:
        tx_hash = call.transact({"from": self.address})
        if self._web3.eth.wait_for_transaction_receipt(tx_hash)["status"] != 1:
            raise RuntimeError(f"transaction {tx_hash.to_0x_hex()} reverted")
        return tx_hash.to_0x_hex()


def _password() -> str:
    path = os.environ.get("ETH_PASSWORD")
    if path is None:
        return getpass.getpass("keystore password: ")
    return Path(path).read_text(encoding="utf-8").rstrip()


class ProductApiError(Exception):
    def __init__(self, status: int, body: str) -> None:
        super().__init__(f"product answered {status}: {body[:200]}")
        self.status = status


class ProductApi:
    """The deposit driver's client for the product's account API, signed with the driver key."""

    def __init__(self, public_url: str, signer: RequestSigner) -> None:
        self._base = public_url.rstrip("/")
        self._http = httpx.Client(auth=SigningAuth(signer), timeout=60)

    def __enter__(self) -> ProductApi:
        return self

    def __exit__(self, *_: object) -> None:
        self._http.close()

    def register(self, team: str) -> str:
        return str(self._call("POST", "/accounts", {"account_id": team})["address"])

    def quote(self, team: str, lock_ref: str, amount_minor: int) -> RateLockResponse:
        body = {"lock_ref": lock_ref, "amount_minor": amount_minor}
        return RateLockResponse.from_dict(
            self._call("POST", f"/accounts/{quote(team)}/quotes", body)
        )

    def account(self, team: str) -> dict[str, Any]:
        return self._call("GET", f"/accounts/{quote(team)}")

    def refund(
        self, team: str, deposit_id: str, to_address: str, amount_atomic: str
    ) -> dict[str, Any]:
        body = {"to_address": to_address, "amount_atomic": amount_atomic}
        return self._call(
            "POST", f"/accounts/{quote(team)}/deposits/{quote(deposit_id)}/refunds", body
        )

    def _call(self, method: str, path: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        response = self._http.request(method, self._base + path, json=body)
        if response.status_code != HTTPStatus.OK:
            raise ProductApiError(response.status_code, response.text)
        value = response.json()
        if not isinstance(value, dict):
            raise ProductApiError(response.status_code, "not a JSON object")
        return value


def run_deposit(
    config: ProductConfig,
    driver: RequestSigner,
    *,
    amount_minor: int,
    min_atomic: int = 0,
    until: str = "credited",
    timeout: float = 1800,
    pay_bps: int = 10_000,
    pay_after_expiry: bool = False,
    persistent_atomic: int | None = None,
    token: str | None = None,
    refund_to: str | None = None,
) -> None:
    """Registers a workspace through the product, pays one deposit, and checks its outcome.

    By default it pays the exact amount of a fresh quote and expects exactly the quoted credit
    at the lock price. `pay_bps` pays that fraction of the quote instead, `pay_after_expiry`
    pays it after the quote's window, and `persistent_atomic` pays the workspace's persistent
    address without a quote; those deposits must be credited at spot. `token` pays another
    token. `until` is `credited` or `swept` for a credit, `rejected` for a rejection, or
    `refunded`: a rejection, then a refund request to `refund_to` for the whole deposit, which
    finance approves and executes (deploy/runbooks/refund-execution.md) while this waits for the
    `deposit.refunded` webhook.
    """
    payer = Payer(config)
    with ProductApi(config.public_url, driver) as api:
        team = f"team-{uuid.uuid4().hex[:12]}"
        persistent = api.register(team)
        LOG.info("registered workspace %s (persistent address %s)", team, persistent)

        lock: RateLockResponse | None = None
        lock_ref = None
        if persistent_atomic is not None:
            address, amount_atomic = persistent, persistent_atomic
        else:
            lock_ref = f"checkout-{uuid.uuid4().hex[:12]}"
            lock = api.quote(team, lock_ref, amount_minor)
            # Pay only an address recomputed here from the product slug, workspace, and lock_ref.
            if not same_address(quote_address(config, team, lock_ref), lock.address):
                raise RuntimeError("quote address does not match the driver's own computation")
            address = lock.address
            amount_atomic = int(lock.amount_atomic) * pay_bps // 10_000
            LOG.info(
                "quote: pay %s atomic to %s before %s for %s minor (%s)",
                lock.amount_atomic,
                lock.address,
                lock.expires_at.isoformat(),
                lock.credit_minor,
                lock.eip681_uri,
            )
        if amount_atomic < min_atomic:
            hint = ""
            if lock is not None:
                needed = -(-amount_minor * min_atomic // amount_atomic)
                hint = f"; rerun with --amount-minor of at least {needed}"
            raise RuntimeError(
                f"the payment would be {amount_atomic} atomic, below --min-atomic {min_atomic}; "
                f"nothing was paid{hint}"
            )
        if lock is not None and pay_after_expiry:
            wait_s = lock.expires_at.timestamp() + LATE_MARGIN_S - time.time()
            LOG.info("waiting %.0fs to pay after the quote window", max(wait_s, 0))
            time.sleep(max(wait_s, 0))

        tx_hash = payer.mint_and_transfer(token or config.token, address, amount_atomic)
        LOG.info("paid %s atomic in %s from %s", amount_atomic, tx_hash, payer.address)
        if until in {"rejected", "refunded"}:
            _check_rejection(api, team, address, refund_to, timeout)
            return
        at_lock_price = lock is not None and pay_bps == 10_000 and not pay_after_expiry
        states = {"credited", "swept"} if until == "credited" else {"swept"}
        expired_ref = lock_ref if pay_after_expiry else None
        deposit, view = _wait_for_account(
            api, team, timeout, lambda view: _credited(view, address, states, expired_ref)
        )
        credited = _event(view, "deposit.credited", deposit_id=str(deposit.id))
        confirmed = _event(view, "deposit.confirmed", deposit_id=str(deposit.id))
        if confirmed["price_source"] != ("lock" if at_lock_price else "spot"):
            raise RuntimeError(f"deposit was valued at the {confirmed['price_source']} price")
        expected_minor = deposit.credit_minor
        if lock is not None and at_lock_price:
            expected_minor = lock.credit_minor
        if credited["amount_minor"] != expected_minor:
            raise RuntimeError("credited amount differs from the expected credit")
        if lock is not None and deposit.lock_ref != lock_ref:
            raise RuntimeError("deposit does not reference its quote")
        credits = [(c["provider_order_id"], c["amount_minor"]) for c in view["credits"]]
        if credits != [(f"deposit:{deposit.id}", int(credited["amount_minor"]))]:
            raise RuntimeError(f"unexpected product ledger credits: {credits}")
        LOG.info(
            "deposit %s is %s: credited %s minor at the %s price (quoted %s; transaction %s); "
            "the ledger holds one credit",
            deposit.id,
            deposit.state,
            credited["amount_minor"],
            confirmed["price_source"],
            None if lock is None else lock.credit_minor,
            credited["destination_tx_id"],
        )


def _check_rejection(
    api: ProductApi, team: str, address: str, refund_to: str | None, timeout: float
) -> None:
    """Waits for the deposit's rejection; with `refund_to`, requests and awaits its refund."""
    deposit, view = _wait_for_account(api, team, timeout, lambda view: _rejected(view, address))
    rejected = _event(view, "deposit.rejected", deposit_id=str(deposit.id))
    if view["credits"]:
        raise RuntimeError(f"a rejected deposit was credited: {view['credits']}")
    LOG.info("deposit %s is rejected (%s); nothing was credited", deposit.id, rejected["reason"])
    if refund_to is None:
        return
    refund = api.refund(team, str(deposit.id), refund_to, deposit.amount_atomic)
    LOG.info(
        "refund %s is %s: %s atomic to %s; approve and execute it with "
        "deploy/runbooks/refund-execution.md (REFUND_ID=%s)",
        refund["id"],
        refund["status"],
        refund["amount_atomic"],
        refund["to_address"],
        refund["id"],
    )
    _, view = _wait_for_account(
        api,
        team,
        timeout,
        lambda view: (
            (deposit, view)
            if _find_event(view, "deposit.refunded", refund_id=refund["id"])
            else None
        ),
    )
    refunded = _event(view, "deposit.refunded", refund_id=refund["id"])
    if refunded["amount_atomic"] != refund["amount_atomic"] or not same_address(
        refunded["to_address"], refund["to_address"]
    ):
        raise RuntimeError(f"deposit.refunded differs from the request: {refunded}")
    LOG.info("refund %s is confirmed in %s", refund["id"], refunded["tx_hash"])


def _deposit_at(view: dict[str, Any], address: str) -> DepositResponse | None:
    for item in view["deposits"]:
        deposit = DepositResponse.from_dict(item)
        if same_address(deposit.address, address):
            return deposit
    return None


def _find_event(view: dict[str, Any], event_type: str, **fields: str) -> dict[str, Any] | None:
    for event in view["events"]:
        data = event["data"]
        if event["type"] == event_type and all(data.get(k) == v for k, v in fields.items()):
            return dict(data)
    return None


def _event(view: dict[str, Any], event_type: str, **fields: str) -> dict[str, Any]:
    event = _find_event(view, event_type, **fields)
    if event is None:
        raise RuntimeError(f"no {event_type} webhook for {fields}")
    return event


def _credited(
    view: dict[str, Any], address: str, states: set[str], expired_lock_ref: str | None
) -> tuple[DepositResponse, dict[str, Any]] | None:
    """Ready once the deposit is in `states` with its `deposit.confirmed` and
    `deposit.credited` webhooks (and `rate_lock.expired` for a late payment) recorded."""
    deposit = _deposit_at(view, address)
    if deposit is None or deposit.state not in states:
        return None
    for event_type in ("deposit.confirmed", "deposit.credited"):
        if _find_event(view, event_type, deposit_id=str(deposit.id)) is None:
            return None
    if expired_lock_ref is not None and (
        _find_event(view, "rate_lock.expired", product_lock_ref=expired_lock_ref) is None
    ):
        return None
    return deposit, view


def _rejected(view: dict[str, Any], address: str) -> tuple[DepositResponse, dict[str, Any]] | None:
    deposit = _deposit_at(view, address)
    if deposit is not None and deposit.state in {"credited", "swept"}:
        raise RuntimeError(f"deposit {deposit.id} is {deposit.state}, not rejected")
    if deposit is None or deposit.state != "rejected":
        return None
    if _find_event(view, "deposit.rejected", deposit_id=str(deposit.id)) is None:
        return None
    return deposit, view


def _wait_for_account(
    api: ProductApi,
    team: str,
    timeout: float,
    ready: Callable[[dict[str, Any]], tuple[DepositResponse, dict[str, Any]] | None],
) -> tuple[DepositResponse, dict[str, Any]]:
    """Polls the product's view of the workspace until `ready` returns a result."""
    deadline = time.monotonic() + timeout
    states: dict[str, str] = {}
    while time.monotonic() < deadline:
        try:
            view = api.account(team)
        except (httpx.HTTPError, ProductApiError) as error:
            if isinstance(error, ProductApiError) and error.status < 500:
                raise
            LOG.warning("product unavailable: %s", error)
            time.sleep(5)
            continue
        for item in view["deposits"]:
            if states.get(item["id"]) != item["state"]:
                LOG.info("deposit %s is %s", item["id"], item["state"])
                states[item["id"]] = item["state"]
        result = ready(view)
        if result is not None:
            return result
        time.sleep(5)
    raise TimeoutError(f"workspace {team} did not reach the expected state in {timeout:.0f}s")
