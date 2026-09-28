"""A stand-in for the Phala Pay service in the demo's end-to-end test.

It answers the merchant API the demo uses, in the shapes of crates/topup/openapi.json: quotes and
their public view, deposit addresses and their public view, deposits, refunds (`mark_paid`
verified on chain), forwarders, the balance, sweeps, attestation, and the TLS evidence. It follows
real payments on Anvil the way the service does, compressed in time (one block a second):

- a transfer to an issued address is a `seen` payment as soon as it is in a block;
- at `CREDIT_DEPTH` confirmations it is recorded as a deposit, valued (the quote's locked price
  when it pays an open quote exactly, otherwise spot), and credited with a signed
  `deposit.credited` webhook;
- at `FINAL_DEPTH` confirmations the deposit is `final`, and refunds may be requested;
- a refund marked paid is verified once its transaction is at `FINAL_DEPTH`: a `Transfer` of the
  deposit's token from the address's treasury to the destination for exactly the amount, in a log
  no other refund used; then `succeeded` with `deposit.refunded`, or `failed` with its
  `failure_reason` and `refund.failed`;
- the service never sweeps: `Flushed` events of the factory, from whoever sent the flush, are
  indexed once at `FINAL_DEPTH` as sweeps and mark the forwarder's earlier deposits `swept`.

`POST /_test/deposits/{id}/reverse` makes a deposit `reversed`, as the service's finality watch
does for a proven-dropped transaction, and sends `deposit.reversed`. Addresses, deposit ids, the
webhook signature, and the attestation binding use the SDK's own helpers, so the product checks
them exactly as it checks the real service.

    python fake_service.py --port 8545 --rpc http://127.0.0.1:8546 --token 0x… \\
        --product-webhook http://127.0.0.1:8089/webhooks --webhook-seed <64 hex> \\
        --factory 0x… --implementation 0x… --account acct_… --treasury 0x…
"""

from __future__ import annotations

import argparse
import json
import logging
import secrets
import threading
import time
import uuid
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import parse_qs, urlsplit

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from topup_sdk import attestation_report_data, credited_event_id, sign_webhook
from topup_sdk.addresses import (
    deposit_address_salt,
    deposit_id,
    forwarder_address,
    keccak256,
    quote_salt,
)

LOG = logging.getLogger("fake_service")
TRANSFER_TOPIC = "0x" + keccak256(b"Transfer(address,address,uint256)").hex()
FLUSHED_TOPIC = "0x" + keccak256(b"Flushed(bytes32,address,address,address,uint256)").hex()
CHAIN_ID = 11155111
PRICE = "0.25000000"  # USD per token
CREDIT_DEPTH = 2
FINAL_DEPTH = 12
MIN_REFUND_ATOMIC = 20 * 10**18  # the staging route's min_refund_atomic
NAMESPACE = uuid.UUID("5b0c6f7e-0f3a-4c9e-9d1e-2f3a4b5c6d7e")
DOCS = "https://phala-network.github.io/phala-pay/#section/Errors/"


class RefusedError(Exception):
    """A request the service answers with a documented error."""

    def __init__(self, status: HTTPStatus, code: str, param: str | None = None) -> None:
        super().__init__(code)
        self.status = status
        self.code = code
        self.param = param


def _topic(address: str) -> str:
    return "0x" + "0" * 24 + address.lower().removeprefix("0x")


def _address(topic: str) -> str:
    return "0x" + topic[-40:].lower()


def _public(value: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in value.items() if not k.startswith("_")}


def _page(url: str, data: list[dict[str, Any]]) -> dict[str, Any]:
    return {"object": "list", "url": url, "has_more": False, "data": data}


class FakeTopup:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.treasury = args.treasury.lower()
        self.token = args.token.lower()
        self.key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(args.webhook_seed))
        self.rpc = httpx.Client(timeout=10)
        self.stop = threading.Event()
        self.lock = threading.RLock()
        self.quotes: dict[str, dict[str, Any]] = {}
        self.addresses: dict[str, dict[str, Any]] = {}  # deposit addresses by id
        self.secrets: dict[str, list[str]] = {}
        self.forwarders: dict[str, dict[str, Any]] = {}  # by lowercase address
        self.seen: dict[str, dict[str, Any]] = {}  # transfers by deposit id
        self.deposits: dict[str, dict[str, Any]] = {}
        self.refunds: dict[str, dict[str, Any]] = {}
        self.sweeps: list[dict[str, Any]] = []
        self.outbox: list[dict[str, Any]] = []
        self.idempotent: dict[str, dict[str, Any]] = {}
        self.scanned = 0
        self.flushes_scanned = 0

    # Chain ------------------------------------------------------------------------------------

    def call(self, method: str, *params: Any) -> Any:
        body = self.rpc.post(
            self.args.rpc, json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).json()
        if "error" in body:
            raise RuntimeError(f"{method}: {body['error']}")
        return body["result"]

    def head(self) -> int:
        return int(self.call("eth_blockNumber"), 16)

    def watch(self) -> None:
        while not self.stop.wait(1.0):
            try:
                self.call("evm_mine")
                self.step()
            except (httpx.HTTPError, RuntimeError):
                LOG.exception("watch step failed")

    def step(self) -> None:
        head = self.head()
        self.detect(head)
        with self.lock:
            for transfer in list(self.seen.values()):
                if (
                    transfer["id"] not in self.deposits
                    and self.depth(transfer, head) >= CREDIT_DEPTH
                ):
                    self.record(transfer)
            for deposit in self.deposits.values():
                deep = head - deposit["block_number"] + 1 >= FINAL_DEPTH
                if deep and deposit["status"] != "reversed" and not deposit["final"]:
                    deposit["final"] = True
                    deposit["final_at"] = int(time.time())
        self.verify_refunds(head)
        self.index_sweeps(head)
        self.deliver()

    @staticmethod
    def depth(transfer: dict[str, Any], head: int) -> int:
        return head - int(transfer["block"]) + 1

    def detect(self, head: int) -> None:
        """Every transfer of the token to an issued address, in the blocks not scanned yet."""
        with self.lock:
            watched = [_topic(address) for address in self.forwarders]
        if not watched or head <= self.scanned:
            return
        logs = self.call(
            "eth_getLogs",
            {
                "address": self.args.token,
                "fromBlock": hex(self.scanned + 1),
                "toBlock": hex(head),
                "topics": [TRANSFER_TOPIC, None, watched],
            },
        )
        for log in logs:
            receipt = self.call("eth_getTransactionReceipt", log["transactionHash"])
            position = next(
                index
                for index, item in enumerate(receipt["logs"])
                if item["logIndex"] == log["logIndex"]
            )
            key = deposit_id(CHAIN_ID, log["transactionHash"], position)
            with self.lock:
                self.seen.setdefault(
                    key,
                    {
                        "id": key,
                        "address": _address(log["topics"][2]),
                        "from": _address(log["topics"][1]),
                        "amount_atomic": str(int(log["data"], 16)),
                        "tx_hash": log["transactionHash"],
                        "block": int(log["blockNumber"], 16),
                        "log_index": int(log["logIndex"], 16),
                        "created": int(time.time()),
                    },
                )
        self.scanned = head

    def record(self, transfer: dict[str, Any]) -> None:
        """Records a transfer at the credit depth as a deposit, values it, and credits it."""
        forwarder = self.forwarders[transfer["address"]]
        quote = self.quotes.get(forwarder["quote"] or "")
        owner = quote or self.addresses[forwarder["deposit_address"]]
        atomic = int(transfer["amount_atomic"])
        at_quote = (
            quote is not None
            and quote["status"] == "open"
            and time.time() < quote["expires_at"]
            and transfer["amount_atomic"] == quote["amount_atomic"]
        )
        now = int(time.time())
        deposit = {
            "id": transfer["id"],
            "object": "deposit",
            "livemode": False,
            "client_reference_id": owner["client_reference_id"],
            "quote": None if quote is None else quote["id"],
            "deposit_address": forwarder["deposit_address"],
            "status": "credited",
            "final": False,
            "final_at": None,
            "swept": False,
            "metadata": dict(owner["metadata"]),
            "rejection_reason": None,
            "chain_id": CHAIN_ID,
            "asset": "pha",
            "asset_contract": self.token,
            "amount_atomic": transfer["amount_atomic"],
            "amount": quote["amount"] if at_quote and quote else atomic * 25 // 10**18,
            "currency": "usd",
            "exchange_rate": PRICE,
            "price_source": "quote" if at_quote else "spot",
            "valued_at": now,
            "address": transfer["address"],
            "from_address": transfer["from"],
            "tx_hash": transfer["tx_hash"],
            "log_index": transfer["log_index"],
            "block_number": transfer["block"],
            "amount_refunded_atomic": "0",
            "refunded": False,
            "amount_refunded": 0,
            "amount_reversed": 0,
            "created": transfer["created"],
        }
        self.deposits[deposit["id"]] = deposit
        if at_quote and quote is not None:
            quote.update(status="complete", deposit=deposit["id"])
        self.emit("deposit.credited", deposit, event_id=credited_event_id(deposit["id"]))

    def verify_refunds(self, head: int) -> None:
        with self.lock:
            marked = [
                r
                for r in self.refunds.values()
                if r["status"] == "pending" and r["transaction_hash"]
            ]
        for refund in marked:
            receipt = self.call("eth_getTransactionReceipt", refund["transaction_hash"])
            if receipt is None or head - int(receipt["blockNumber"], 16) + 1 < FINAL_DEPTH:
                continue
            with self.lock:
                reason = self.match(refund, receipt)
                if reason is None:
                    self.succeed(refund)
                else:
                    refund.update(status="failed", failure_reason=reason)
                    self.emit("refund.updated", refund)
                    self.emit("refund.failed", refund)

    def match(self, refund: dict[str, Any], receipt: dict[str, Any]) -> str | None:
        """The refund's `failure_reason` for the finalized receipt, or `None` when it pays it."""
        if int(receipt["status"], 16) != 1:
            return "transaction_failed"
        deposit = self.deposits[refund["deposit"]]
        logs = [
            (position, log)
            for position, log in enumerate(receipt["logs"])
            if log["address"].lower() == deposit["asset_contract"]
            and log["topics"]
            and log["topics"][0] == TRANSFER_TOPIC
        ]
        if refund["receipt_log_index"] is not None:
            logs = [(p, log) for p, log in logs if p == refund["receipt_log_index"]]
        if not logs:
            return "transfer_not_found"
        position, log = logs[0]
        if _address(log["topics"][1]) != refund["treasury"]:
            return "sender_mismatch"
        if _address(log["topics"][2]) != refund["destination_address"]:
            return "destination_mismatch"
        if str(int(log["data"], 16)) != refund["amount_atomic"]:
            return "amount_mismatch"
        used = {
            (other["transaction_hash"], other["receipt_log_index"])
            for other in self.refunds.values()
            if other["status"] == "succeeded"
        }
        if (refund["transaction_hash"], position) in used:
            return "transfer_already_used"
        refund["receipt_log_index"] = position
        return None

    def succeed(self, refund: dict[str, Any]) -> None:
        deposit = self.deposits[refund["deposit"]]
        refunded = int(deposit["amount_refunded_atomic"]) + int(refund["amount_atomic"])
        atomic = int(deposit["amount_atomic"])
        deposit.update(
            amount_refunded_atomic=str(refunded),
            amount_refunded=(deposit["amount"] or 0) * refunded // atomic,
            refunded=refunded == atomic,
        )
        refund["status"] = "succeeded"
        self.emit("refund.updated", refund)
        self.emit("deposit.refunded", deposit)

    def index_sweeps(self, head: int) -> None:
        """Indexes the factory's finalized `Flushed` events, whoever sent the flush."""
        final = head - FINAL_DEPTH + 1
        if final <= self.flushes_scanned:
            return
        logs = self.call(
            "eth_getLogs",
            {
                "address": self.args.factory,
                "fromBlock": hex(self.flushes_scanned + 1),
                "toBlock": hex(final),
                "topics": [FLUSHED_TOPIC],
            },
        )
        with self.lock:
            for log in logs:
                address = _address(log["topics"][2])
                forwarder = self.forwarders.get(address)
                if forwarder is None:
                    continue
                data = log["data"].removeprefix("0x")
                block, index = int(log["blockNumber"], 16), int(log["logIndex"], 16)
                sweep = {
                    "id": "sw_" + uuid.uuid5(NAMESPACE, f"{log['transactionHash']}:{index}").hex,
                    "object": "sweep",
                    "livemode": False,
                    "chain_id": CHAIN_ID,
                    "forwarder": forwarder["id"],
                    "address": address,
                    "token": _address(log["topics"][3]),
                    "asset": "pha",
                    "treasury": _address(data[:64]),
                    "amount_atomic": str(int(data[64:128], 16)),
                    "tx_hash": log["transactionHash"],
                    "log_index": index,
                    "block_number": block,
                    "created": int(time.time()),
                }
                self.sweeps.insert(0, sweep)
                for deposit in self.deposits.values():
                    if deposit["address"] == address and (
                        (deposit["block_number"], deposit["log_index"]) < (block, index)
                    ):
                        deposit["swept"] = True
        self.flushes_scanned = final

    # Webhooks ---------------------------------------------------------------------------------

    def emit(self, event_type: str, obj: dict[str, Any], *, event_id: str | None = None) -> None:
        """Queues an event whose `data.object` is rendered now, as the service's outbox does."""
        self.outbox.append(
            {
                "id": event_id or "evt_" + uuid.uuid4().hex,
                "object": "event",
                "account": self.args.account,
                "livemode": False,
                "type": event_type,
                "created": int(time.time()),
                "request": None,
                "data": {"object": json.loads(json.dumps(_public(obj)))},
                "_delivered": False,
            }
        )

    def deliver(self) -> None:
        """Sends every undelivered event until the product answers `2xx`."""
        with self.lock:
            pending = [event for event in self.outbox if not event["_delivered"]]
        for event in pending:
            body = json.dumps(_public(event)).encode()
            headers = sign_webhook(self.key, event["id"], int(time.time()), body)
            try:
                response = httpx.post(
                    self.args.product_webhook,
                    content=body,
                    headers={**headers, "content-type": "application/json"},
                    timeout=10,
                )
            except httpx.HTTPError:
                LOG.warning("webhook delivery failed; retrying")
                return
            event["_delivered"] = response.is_success

    # Quotes -----------------------------------------------------------------------------------

    def add_forwarder(
        self, address: str, salt: bytes, *, quote: str | None, deposit_address: str | None
    ) -> None:
        self.forwarders[address.lower()] = {
            "id": "fwd_" + uuid.uuid5(NAMESPACE, address.lower()).hex,
            "object": "forwarder",
            "livemode": False,
            "chain_id": CHAIN_ID,
            "address": address,
            "factory": self.args.factory,
            "salt": "0x" + salt.hex(),
            "treasury": self.treasury,
            "quote": quote,
            "deposit_address": deposit_address,
            "superseded_at": None,
        }

    def create_quote(self, body: dict[str, Any], idempotency_key: str) -> dict[str, Any]:
        quote_id = "qt_" + uuid.uuid5(uuid.NAMESPACE_OID, idempotency_key).hex
        customer = str(body["client_reference_id"])
        amount = int(body["amount"])
        salt = quote_salt(self.args.account, customer, quote_id)
        address = forwarder_address(
            self.args.factory, self.args.implementation, self.treasury, salt
        )
        atomic = str(amount * 4 * 10**16)  # cents / 100 / 0.25 USD per token * 10**18
        now = int(time.time())
        with self.lock:
            quote = self.quotes.get(quote_id)
            if quote is None:
                quote = {
                    "id": quote_id,
                    "object": "quote",
                    "livemode": False,
                    "client_reference_id": customer,
                    "treasury": self.treasury,
                    "metadata": dict(body.get("metadata") or {}),
                    "amount": amount,
                    "currency": "usd",
                    "chain_id": CHAIN_ID,
                    "asset": "pha",
                    "amount_atomic": atomic,
                    "exchange_rate": PRICE,
                    "address": address,
                    "payment_uri": f"ethereum:{self.args.token}@{CHAIN_ID}/transfer"
                    f"?address={address}&uint256={atomic}",
                    "status": "open",
                    "expires_at": now + 900,
                    "created": now,
                    "deposit": None,
                }
                self.quotes[quote_id] = quote
                self.add_forwarder(address, salt, quote=quote_id, deposit_address=None)
            secret = f"{quote_id}_secret_{secrets.token_hex(24)}"
            self.secrets.setdefault(quote_id, []).append(secret)
            return {**self.quote_view(quote), "client_secret": secret}

    def transfers_at(self, address: str) -> list[dict[str, Any]]:
        return sorted(
            (t for t in self.seen.values() if t["address"] == address.lower()),
            key=lambda t: (t["block"], t["log_index"]),
        )

    def payment(self, transfer: dict[str, Any], quote: dict[str, Any] | None) -> dict[str, Any]:
        """A transfer as the merchant's `Payment`: `seen`, then `recorded` as a deposit."""
        recorded = transfer["id"] in self.deposits
        return {
            "status": "recorded" if recorded else "seen",
            "chain_id": CHAIN_ID,
            "asset": "pha",
            "tx_hash": transfer["tx_hash"],
            "amount_atomic": transfer["amount_atomic"],
            "confirmations": None if recorded else self.depth(transfer, self.head()),
            "estimated_final_at": None if recorded else transfer["created"] + 60,
            "matches_quote": None
            if quote is None
            else transfer["amount_atomic"] == quote["amount_atomic"],
            "deposit": transfer["id"],
        }

    def quote_view(self, quote: dict[str, Any]) -> dict[str, Any]:
        transfers = self.transfers_at(quote["address"])
        shown = next((t for t in transfers if t["id"] == quote["deposit"]), None)
        shown = shown or (transfers[0] if transfers else None)
        return {**quote, "payment": None if shown is None else self.payment(shown, quote)}

    def client_quote(self, quote: dict[str, Any]) -> dict[str, Any]:
        transfers = self.transfers_at(quote["address"])
        payment_status, confirmations = "none", None
        if quote["deposit"] is not None or any(t["id"] in self.deposits for t in transfers):
            payment_status = "credited"
        elif transfers:
            payment_status = "seen"
            confirmations = self.depth(transfers[0], self.head())
        keys = (
            *("id", "object", "status", "amount", "currency", "asset", "chain_id"),
            *("amount_atomic", "address", "payment_uri", "expires_at"),
        )
        view = {k: quote[k] for k in keys}
        view.update(
            livemode=False, decimals=18, payment_status=payment_status, confirmations=confirmations
        )
        return view

    # Deposit addresses ------------------------------------------------------------------------

    def create_deposit_address(self, body: dict[str, Any]) -> dict[str, Any]:
        customer = str(body["client_reference_id"])
        with self.lock:
            address = next(
                (
                    a
                    for a in self.addresses.values()
                    if a["client_reference_id"] == customer and a["status"] == "active"
                ),
                None,
            )
            if address is None:
                salt = deposit_address_salt(
                    self.args.account, livemode=False, client_reference_id=customer, version=1
                )
                at = forwarder_address(
                    self.args.factory, self.args.implementation, self.treasury, salt
                )
                address = {
                    "id": "da_" + uuid.uuid4().hex,
                    "object": "deposit_address",
                    "livemode": False,
                    "client_reference_id": customer,
                    "address": at,
                    "version": 1,
                    "salt": "0x" + salt.hex(),
                    "status": "active",
                    "created": int(time.time()),
                    "retired_at": None,
                    "metadata": {},
                    "networks": [
                        {
                            "chain_id": CHAIN_ID,
                            "address": at,
                            "treasury": self.treasury,
                            "assets": [
                                {
                                    "asset": "pha",
                                    "contract": self.token,
                                    "decimals": 18,
                                    "payment_uri": f"ethereum:{self.token}@{CHAIN_ID}/transfer"
                                    f"?address={at}",
                                }
                            ],
                        }
                    ],
                }
                self.addresses[address["id"]] = address
                self.add_forwarder(at, salt, quote=None, deposit_address=address["id"])
            # The create request's metadata merges into the address's, as an update would.
            address["metadata"].update(body.get("metadata") or {})
            secret = f"{address['id']}_secret_{secrets.token_hex(24)}"
            self.secrets.setdefault(address["id"], []).append(secret)
            return {**self.deposit_address_view(address), "client_secret": secret}

    def deposit_address_view(self, address: dict[str, Any]) -> dict[str, Any]:
        transfers = self.transfers_at(address["address"])[::-1][:10]
        return {**address, "payments": [self.payment(t, None) for t in transfers]}

    def client_deposit_address(self, address: dict[str, Any]) -> dict[str, Any]:
        payments = []
        for transfer in self.transfers_at(address["address"])[::-1][:10]:
            deposit = self.deposits.get(transfer["id"])
            status = "seen" if deposit is None else deposit["status"]
            payments.append(
                {
                    "status": status,
                    "chain_id": CHAIN_ID,
                    "asset": "pha",
                    "decimals": 18,
                    "amount_atomic": transfer["amount_atomic"],
                    "tx_hash": transfer["tx_hash"],
                    "confirmations": self.depth(transfer, self.head()) if deposit is None else None,
                    "created": transfer["created"],
                }
            )
        return {
            "id": address["id"],
            "object": "deposit_address",
            "livemode": False,
            "status": address["status"],
            "address": address["address"],
            "networks": [
                {k: network[k] for k in ("chain_id", "address", "assets")}
                for network in address["networks"]
            ],
            "payments": payments,
        }

    # Refunds ----------------------------------------------------------------------------------

    def create_refund(self, body: dict[str, Any]) -> dict[str, Any]:
        with self.lock:
            deposit = self.deposits.get(str(body.get("deposit")))
            if deposit is None:
                raise RefusedError(HTTPStatus.BAD_REQUEST, "parameter_invalid", "deposit")
            if not deposit["final"]:
                raise RefusedError(HTTPStatus.BAD_REQUEST, "deposit_not_final")
            if deposit["status"] not in ("credited", "rejected"):
                raise RefusedError(HTTPStatus.BAD_REQUEST, "deposit_not_refundable")
            reserved = sum(
                int(r["amount_atomic"])
                for r in self.refunds.values()
                if r["deposit"] == deposit["id"] and r["status"] in ("pending", "succeeded")
            )
            remainder = int(deposit["amount_atomic"]) - reserved
            amount = int(body.get("amount_atomic") or remainder)
            if amount < MIN_REFUND_ATOMIC or remainder <= 0:
                raise RefusedError(HTTPStatus.BAD_REQUEST, "amount_too_small", "amount_atomic")
            if amount > remainder:
                raise RefusedError(HTTPStatus.BAD_REQUEST, "amount_too_large", "amount_atomic")
            refund = {
                "id": "re_" + uuid.uuid4().hex,
                "object": "refund",
                "livemode": False,
                "deposit": deposit["id"],
                "amount_atomic": str(amount),
                "destination_address": str(body["destination_address"]).lower(),
                # The treasury the deposit's address pays, which the refund must come from.
                "treasury": self.forwarders[deposit["address"]]["treasury"],
                "status": "pending",
                "failure_reason": None,
                "transaction_hash": None,
                "receipt_log_index": None,
                "created": int(time.time()),
                "metadata": dict(body.get("metadata") or {}),
            }
            self.refunds[refund["id"]] = refund
            self.emit("refund.created", refund)
            return refund

    def mark_paid(self, refund_id: str, body: dict[str, Any]) -> dict[str, Any]:
        with self.lock:
            refund = self.refund(refund_id)
            tx_hash = str(body["transaction_hash"]).lower()
            if refund["transaction_hash"] == tx_hash:
                return refund
            if refund["status"] != "pending" or refund["transaction_hash"] is not None:
                raise RefusedError(HTTPStatus.BAD_REQUEST, "refund_unexpected_state")
            index = body.get("receipt_log_index")
            refund.update(transaction_hash=tx_hash, receipt_log_index=index)
            self.emit("refund.updated", refund)
            return refund

    def cancel_refund(self, refund_id: str) -> dict[str, Any]:
        with self.lock:
            refund = self.refund(refund_id)
            if refund["status"] == "canceled":
                return refund
            if refund["status"] != "pending" or refund["transaction_hash"] is not None:
                raise RefusedError(HTTPStatus.BAD_REQUEST, "refund_unexpected_state")
            refund["status"] = "canceled"
            self.emit("refund.updated", refund)
            return refund

    def refund(self, refund_id: str) -> dict[str, Any]:
        refund = self.refunds.get(refund_id)
        if refund is None:
            raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")
        return refund

    def reverse(self, deposit_id: str) -> dict[str, Any]:
        """The finality watch's outcome for a proven-dropped transaction."""
        with self.lock:
            deposit = self.deposits.get(deposit_id)
            if deposit is None:
                raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")
            if deposit["final"] or deposit["status"] == "reversed":
                raise RefusedError(HTTPStatus.BAD_REQUEST, "deposit_final")
            deposit.update(status="reversed", amount_reversed=deposit["amount"] or 0)
            for refund in self.refunds.values():
                if (
                    refund["deposit"] == deposit_id
                    and refund["status"] == "pending"
                    and refund["transaction_hash"] is None
                ):
                    refund["status"] = "canceled"
                    self.emit("refund.updated", refund)
            self.emit("deposit.reversed", deposit)
            return deposit

    # Balance, forwarders, sweeps --------------------------------------------------------------

    def unswept(self, deposit: dict[str, Any]) -> bool:
        return deposit["status"] != "reversed" and not deposit["swept"]

    def balance(self) -> dict[str, Any]:
        with self.lock:
            unswept = [d for d in self.deposits.values() if self.unswept(d)]
            total = sum(int(d["amount_atomic"]) for d in unswept)
            final = sum(int(d["amount_atomic"]) for d in unswept if d["final"])
        amounts = [
            {
                "chain_id": CHAIN_ID,
                "token": self.token,
                "asset": "pha",
                "amount_atomic": str(total),
                "final_amount_atomic": str(final),
            }
        ]
        return {"object": "balance", "livemode": False, "unswept": amounts if total else []}

    def list_forwarders(self, query: dict[str, str]) -> list[dict[str, Any]]:
        with self.lock:
            forwarders = list(self.forwarders.values())[::-1]
            sweepable = query.get("sweepable", "").lower()
            if sweepable:
                forwarders = [
                    f
                    for f in forwarders
                    if any(
                        d["address"] == f["address"].lower()
                        and d["asset_contract"] == sweepable
                        and d["final"]
                        and self.unswept(d)
                        for d in self.deposits.values()
                    )
                ]
            return forwarders

    def attestation(self, nonce: str) -> dict[str, Any]:
        public = self.key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        report = attestation_report_data(
            bytes.fromhex(nonce), self.args.account, False, [(1, public)]
        )
        return {
            "object": "attestation",
            "account": self.args.account,
            "livemode": False,
            "webhook_keys": [{"version": 1, "public_key": public.hex(), "expires_at": None}],
            "report_data": report.hex(),
            "tdx_quote": "00" * 1024,
        }


def evidence() -> dict[str, Any]:
    events = [
        ("app-id", "e2e0000000000000000000000000000000000001"),
        ("compose-hash", "c0" * 32),
        ("os-image-hash", "05" * 32),
    ]
    return {
        "quote": "00" * 1024,
        "report_data": "00" * 64,
        "vm_config": "{}",
        "event_log": json.dumps(
            [{"imr": 3, "event": name, "event_payload": value} for name, value in events]
        ),
    }


def serve(fake: FakeTopup) -> ThreadingHTTPServer:
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            self.dispatch(self.get)

        def do_POST(self) -> None:
            self.dispatch(self.post)

        def dispatch(self, route: Any) -> None:
            url = urlsplit(self.path)
            query = {k: v[0] for k, v in parse_qs(url.query).items()}
            parts = url.path.strip("/").split("/")
            try:
                route(url.path, parts, query)
            except RefusedError as refused:
                self.error(refused.status, refused.code, refused.param)

        def merchant(self) -> bool:
            return self.headers.get("authorization", "").startswith("Bearer ppay_")

        def get(self, path: str, parts: list[str], query: dict[str, str]) -> None:
            if path == "/evidences/quote.json":
                self.send(HTTPStatus.OK, evidence())
                return
            if parts[:2] == ["v1", "quotes"] and len(parts) == 3:
                quote = fake.quotes.get(parts[2])
                if quote is None:
                    raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")
                if not self.merchant():
                    if query.get("client_secret") not in fake.secrets.get(quote["id"], []):
                        raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")
                    with fake.lock:
                        self.send(HTTPStatus.OK, fake.client_quote(quote), cors=True)
                    return
                with fake.lock:
                    self.send(HTTPStatus.OK, fake.quote_view(quote))
                return
            if parts[:2] == ["v1", "deposit_addresses"] and len(parts) == 3:
                address = fake.addresses.get(parts[2])
                if address is None:
                    raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")
                if not self.merchant():
                    if query.get("client_secret") not in fake.secrets.get(address["id"], []):
                        raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")
                    with fake.lock:
                        self.send(HTTPStatus.OK, fake.client_deposit_address(address), cors=True)
                    return
                with fake.lock:
                    self.send(HTTPStatus.OK, fake.deposit_address_view(address))
                return
            if not self.merchant():
                raise RefusedError(HTTPStatus.UNAUTHORIZED, "api_key_missing")
            if path == "/v1/attestation":
                self.send(HTTPStatus.OK, fake.attestation(query["nonce"]))
            elif path == "/v1/deposits":
                with fake.lock:
                    data = [
                        _public(d)
                        for d in sorted(fake.deposits.values(), key=lambda d: -d["created"])
                        if all(
                            query.get(name, d[name]) == d[name]
                            for name in ("quote", "client_reference_id", "deposit_address")
                        )
                    ]
                self.send(HTTPStatus.OK, _page(path, data))
            elif parts[:2] == ["v1", "deposits"] and len(parts) == 3:
                deposit = fake.deposits.get(parts[2])
                if deposit is None:
                    raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")
                self.send(HTTPStatus.OK, _public(deposit))
            elif path == "/v1/refunds":
                with fake.lock:
                    data = [
                        r
                        for r in fake.refunds.values()
                        if query.get("deposit", r["deposit"]) == r["deposit"]
                    ][::-1]
                self.send(HTTPStatus.OK, _page(path, data))
            elif parts[:2] == ["v1", "refunds"] and len(parts) == 3:
                self.send(HTTPStatus.OK, fake.refund(parts[2]))
            elif path == "/v1/balance":
                self.send(HTTPStatus.OK, fake.balance())
            elif path == "/v1/forwarders":
                self.send(HTTPStatus.OK, _page(path, fake.list_forwarders(query)))
            elif path == "/v1/sweeps":
                with fake.lock:
                    data = [
                        s
                        for s in fake.sweeps
                        if query.get("token", s["token"]).lower() == s["token"]
                        and query.get("forwarder", s["forwarder"]) == s["forwarder"]
                    ]
                self.send(HTTPStatus.OK, _page(path, data))
            else:
                raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")

        def post(self, path: str, parts: list[str], _query: dict[str, str]) -> None:
            length = int(self.headers.get("content-length") or 0)
            body = json.loads(self.rfile.read(length) or b"{}")
            if parts[:2] == ["_test", "deposits"] and parts[3:] == ["reverse"]:
                self.send(HTTPStatus.OK, _public(fake.reverse(parts[2])))
                return
            if not self.merchant():
                raise RefusedError(HTTPStatus.UNAUTHORIZED, "api_key_missing")
            key = self.headers.get("idempotency-key", "").strip('"')
            if path == "/v1/quotes":
                self.send(HTTPStatus.OK, fake.create_quote(body, key))
            elif path == "/v1/deposit_addresses":
                self.send(HTTPStatus.OK, fake.create_deposit_address(body))
            elif path == "/v1/refunds":
                with fake.lock:
                    replay = fake.idempotent.get(key) if key else None
                    refund = replay or fake.create_refund(body)
                    if key:
                        fake.idempotent[key] = refund
                self.send(HTTPStatus.OK, refund)
            elif parts[:2] == ["v1", "refunds"] and parts[3:] == ["mark_paid"]:
                self.send(HTTPStatus.OK, fake.mark_paid(parts[2], body))
            elif parts[:2] == ["v1", "refunds"] and parts[3:] == ["cancel"]:
                self.send(HTTPStatus.OK, fake.cancel_refund(parts[2]))
            else:
                raise RefusedError(HTTPStatus.NOT_FOUND, "resource_missing")

        def error(self, status: HTTPStatus, code: str, param: str | None = None) -> None:
            error: dict[str, Any] = {
                "type": "invalid_request_error",
                "code": code,
                "message": code,
                "doc_url": DOCS + code,
            }
            if param is not None:
                error["param"] = param
            self.send(status, {"error": error}, cors=True)

        def send(self, status: HTTPStatus, body: dict[str, Any], *, cors: bool = False) -> None:
            payload = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("request-id", "req_" + uuid.uuid4().hex)
            if cors:
                self.send_header("access-control-allow-origin", "*")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, format: str, *args: Any) -> None:
            LOG.debug(format, *args)

    return ThreadingHTTPServer(("127.0.0.1", fake.args.port), Handler)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    for name in ("rpc", "token", "product-webhook", "webhook-seed", "factory"):
        parser.add_argument(f"--{name}", required=True)
    for name in ("implementation", "account", "treasury"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--port", type=int, required=True)
    logging.basicConfig(level=logging.INFO, format="fake_service %(levelname)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    fake = FakeTopup(parser.parse_args())
    server = serve(fake)
    threading.Thread(target=fake.watch, daemon=True).start()
    try:
        server.serve_forever()
    finally:
        fake.stop.set()


if __name__ == "__main__":
    main()
