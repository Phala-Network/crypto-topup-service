"""The product ledger: SQLite standing in for Phala Cloud's database."""

from __future__ import annotations

import json
import os
import sqlite3
import threading
import time
from collections.abc import Callable, Iterator, Mapping, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any

ORDER_FLOW_CODE = "crypto-top-up"
ORDER_PROVIDER = "crypto_topup"

SCHEMA = """
CREATE TABLE IF NOT EXISTS teams (
    id TEXT PRIMARY KEY,
    suspended INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS team_addresses (
    address TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    kind TEXT NOT NULL,
    version INTEGER,
    lock_ref TEXT
);
-- team_id is NULL only for credits held because they name no known workspace.
CREATE TABLE IF NOT EXISTS orders (
    id TEXT PRIMARY KEY,
    team_id TEXT REFERENCES teams (id),
    provider TEXT NOT NULL,
    order_flow_code TEXT NOT NULL,
    provider_order_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL,
    reason TEXT,
    credit_transaction_id TEXT,
    created_at REAL NOT NULL
);
-- Phala Cloud's partial unique index; the key is also looked up across teams before insert.
CREATE UNIQUE INDEX IF NOT EXISTS orders_crypto_topup_provider_order
    ON orders (team_id, provider_order_id) WHERE order_flow_code = 'crypto-top-up';
CREATE TABLE IF NOT EXISTS credit_transactions (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    order_id TEXT NOT NULL UNIQUE REFERENCES orders (id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
    funding_source TEXT NOT NULL,
    created_at REAL NOT NULL
);
-- The latest merged snapshot of each deposit (`deposit.*` events carry the whole deposit): the
-- later status and the larger cumulative claw-backs win, whatever order events arrive in.
CREATE TABLE IF NOT EXISTS deposit_snapshots (
    provider_order_id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    amount_refunded_minor INTEGER NOT NULL CHECK (amount_refunded_minor >= 0),
    amount_reversed_minor INTEGER NOT NULL CHECK (amount_reversed_minor >= 0)
);
-- Changes to a credited order after its credit: refunds and reversals take it back.
CREATE TABLE IF NOT EXISTS credit_adjustments (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    order_id TEXT NOT NULL REFERENCES orders (id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor <> 0),
    reason TEXT NOT NULL,
    created_at REAL NOT NULL
);
-- The product's own promotion on an accepted order (`bonus_bps`), a line of its own beside the
-- credit: the grant, then its claw-backs as refunds and reversals net the credit down. An order's
-- rows sum to its current bonus. Not a Phala Pay amount: the service never sees it.
CREATE TABLE IF NOT EXISTS bonus_credits (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    order_id TEXT NOT NULL REFERENCES orders (id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor <> 0),
    reason TEXT NOT NULL,
    created_at REAL NOT NULL
);
-- The webhook inbox: every verified delivery once, by its `webhook-id` (the event's `evt_` id),
-- committed with its ledger effect. `data` is the event's parsed `data`, for the product's own
-- reads; `body` and the three Standard Webhooks headers are the delivery exactly as received, the
-- evidence a service restore imports (deploy/runbooks/restore.md, step 5). A ledger from before
-- the inbox gains these columns, NULL on its earlier rows (MIGRATIONS). `state` is `processed`,
-- or `ignored` for a `deposit.*` event whose deposit was malformed.
CREATE TABLE IF NOT EXISTS webhook_events (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    data TEXT NOT NULL,
    received_at REAL NOT NULL
);
-- Each quote and deposit address the product created, as the service returned it, its
-- `client_secret` included: the merchant's records a service restore re-issues them from
-- (deploy/runbooks/restore.md, step 4). A client secret is a capability: the ledger file is
-- readable by its owner only, and nothing logs it.
CREATE TABLE IF NOT EXISTS quote_records (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    response TEXT NOT NULL,
    recorded_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS deposit_address_records (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    response TEXT NOT NULL,
    recorded_at REAL NOT NULL
);
-- Orders credited before prefixed ids were keyed `deposit:<uuid>`; the key is now the deposit's
-- `dep_` id, the same UUID in 32 hex digits, so a replayed old credit is still recognized.
UPDATE orders
SET provider_order_id = 'dep_' || replace(substr(provider_order_id, 9), '-', '')
WHERE provider_order_id LIKE 'deposit:%';
"""


# Columns added to existing tables since their creation, applied once when a ledger lacks them.
MIGRATIONS = {
    "webhook_events": (
        ("body", "BLOB"),
        ("webhook_timestamp", "TEXT"),
        ("webhook_signature", "TEXT"),
        ("state", "TEXT NOT NULL DEFAULT 'processed'"),
        ("processed_at", "REAL"),
    ),
}


@dataclass(frozen=True)
class Delivery:
    """A webhook delivery as the receiver got it: the Standard Webhooks headers and the raw body,
    byte for byte. Only this, not a re-serialized event, verifies against the service's key."""

    webhook_id: str
    webhook_timestamp: str
    webhook_signature: str
    body: bytes


# How far each deposit status is along the deposit's life; a snapshot never moves it back.
STATUS_RANK = {"pending": 0, "credited": 1, "rejected": 1, "reversed": 2}


@dataclass(frozen=True)
class DepositView:
    """A deposit's merged snapshot: its furthest status and cumulative claw-backs, in cents."""

    status: str
    amount_refunded: int
    amount_reversed: int

    def merge(self, other: DepositView) -> DepositView:
        status = (
            other.status
            if STATUS_RANK.get(other.status, 0) > STATUS_RANK.get(self.status, 0)
            else self.status
        )
        return DepositView(
            status,
            max(self.amount_refunded, other.amount_refunded),
            max(self.amount_reversed, other.amount_reversed),
        )

    def contribution(self, credited: int) -> int:
        """What a credit of `credited` cents nets to: less the claw-backs while the deposit is
        `credited` or `reversed` (a reversal takes back all of it), nothing otherwise."""
        if self.status not in ("credited", "reversed"):
            return 0
        return max(0, credited - self.amount_refunded - self.amount_reversed)


@dataclass(frozen=True)
class StoredOrder:
    provider_order_id: str
    team_id: str | None
    payload: dict[str, Any]
    status: str
    reason: str | None
    credit_transaction_id: str | None
    id: str


class ProductLedger:
    """SQLite stand-in for the product database; every write is one serialized transaction."""

    def __init__(self, path: str = ":memory:") -> None:
        if path != ":memory:":
            # The ledger holds client secrets: owner-only, and SQLite gives its journal the
            # database file's mode.
            os.close(os.open(path, os.O_CREAT | os.O_RDWR, 0o600))
            os.chmod(path, 0o600)
        self._connection = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        self._connection.execute("PRAGMA foreign_keys = ON")
        self._connection.executescript(SCHEMA)
        self._lock = threading.RLock()
        self.events_changed = threading.Condition(self._lock)
        with self.transaction() as db:
            for table, added in MIGRATIONS.items():
                columns = {row[1] for row in db.execute(f"PRAGMA table_info({table})")}
                for name, declaration in added:
                    if name not in columns:
                        db.execute(f"ALTER TABLE {table} ADD COLUMN {name} {declaration}")

    @contextmanager
    def transaction(self) -> Iterator[sqlite3.Connection]:
        with self._lock:
            self._connection.execute("BEGIN IMMEDIATE")
            try:
                yield self._connection
            except BaseException:
                self._connection.execute("ROLLBACK")
                raise
            self._connection.execute("COMMIT")

    def add_team(self, team_id: str, *, suspended: bool = False) -> None:
        with self.transaction() as db:
            db.execute(
                "INSERT INTO teams (id, suspended) VALUES (?, ?) "
                "ON CONFLICT (id) DO UPDATE SET suspended = excluded.suspended",
                (team_id, int(suspended)),
            )

    def team_suspended(self, team_id: str) -> bool | None:
        with self._lock:
            row = self._connection.execute(
                "SELECT suspended FROM teams WHERE id = ?", (team_id,)
            ).fetchone()
        return None if row is None else bool(row[0])

    def record_quote(self, team_id: str, quote: Mapping[str, Any]) -> None:
        """Records a quote the service created for the workspace, as it returned it (with its
        `client_secret`), and its address (history only: credits name the account)."""
        with self.transaction() as db:
            self._record_address(db, quote["address"], team_id, quote["id"])
            db.execute(
                "INSERT INTO quote_records (id, team_id, response, recorded_at) "
                "VALUES (?, ?, ?, ?) ON CONFLICT (id) DO NOTHING",
                (quote["id"], team_id, json.dumps(quote, sort_keys=True), time.time()),
            )

    def record_deposit_address(self, team_id: str, address: Mapping[str, Any]) -> None:
        """Records a deposit address the service returned for the workspace, as it returned it,
        and each network's address. A later response (a fresh `client_secret`) replaces it."""
        with self.transaction() as db:
            for network in address["networks"]:
                self._record_address(db, network["address"], team_id, address["id"])
            db.execute(
                "INSERT INTO deposit_address_records (id, team_id, response, recorded_at) "
                "VALUES (?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET "
                "response = excluded.response, recorded_at = excluded.recorded_at",
                (address["id"], team_id, json.dumps(address, sort_keys=True), time.time()),
            )

    @staticmethod
    def _record_address(db: sqlite3.Connection, address: str, team_id: str, ref: str) -> None:
        db.execute(
            "INSERT OR IGNORE INTO team_addresses (address, team_id, kind, version, lock_ref) "
            "VALUES (?, ?, 'lock', NULL, ?)",
            (address.lower(), team_id, ref),
        )

    def quote_records(self) -> list[dict[str, Any]]:
        """Every recorded quote response, oldest first."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT response FROM quote_records ORDER BY recorded_at, id"
            ).fetchall()
        return [json.loads(row[0]) for row in rows]

    def deposit_address_records(self) -> list[dict[str, Any]]:
        """Every recorded deposit address response, oldest first."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT response FROM deposit_address_records ORDER BY recorded_at, id"
            ).fetchall()
        return [json.loads(row[0]) for row in rows]

    def find_order(self, provider_order_id: str) -> StoredOrder | None:
        with self._lock:
            return self._find_order(self._connection, provider_order_id)

    def credited_since(self, db: sqlite3.Connection, team_id: str, since: float) -> int:
        row = db.execute(
            "SELECT COALESCE(SUM(amount_minor), 0) FROM credit_transactions "
            "WHERE team_id = ? AND created_at >= ?",
            (team_id, since),
        ).fetchone()
        return int(row[0])

    def credits_for(self, team_id: str) -> list[tuple[str, int]]:
        with self._lock:
            rows = self._connection.execute(
                "SELECT o.provider_order_id, c.amount_minor FROM credit_transactions c "
                "JOIN orders o ON o.id = c.order_id WHERE c.team_id = ? ORDER BY c.created_at",
                (team_id,),
            ).fetchall()
        return [(str(key), int(amount)) for key, amount in rows]

    def balance_for(self, team_id: str) -> int:
        """The workspace's crypto top-up balance: its credits less their claw-backs, and its
        bonuses less theirs."""
        with self._lock:
            row = self._connection.execute(
                "SELECT (SELECT COALESCE(SUM(amount_minor), 0) FROM credit_transactions "
                "WHERE team_id = ?) + (SELECT COALESCE(SUM(amount_minor), 0) "
                "FROM credit_adjustments WHERE team_id = ?) + (SELECT "
                "COALESCE(SUM(amount_minor), 0) FROM bonus_credits WHERE team_id = ?)",
                (team_id, team_id, team_id),
            ).fetchone()
        return int(row[0])

    def bonuses_for(self, team_id: str) -> list[tuple[str, int, str]]:
        """The workspace's bonus lines: `(provider_order_id, amount_minor, reason)`, oldest
        first."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT o.provider_order_id, b.amount_minor, b.reason FROM bonus_credits b "
                "JOIN orders o ON o.id = b.order_id WHERE b.team_id = ? ORDER BY b.created_at",
                (team_id,),
            ).fetchall()
        return [(str(key), int(amount), str(reason)) for key, amount, reason in rows]

    def adjustments_for(self, team_id: str) -> list[tuple[str, int, str]]:
        """The workspace's claw-backs: `(provider_order_id, amount_minor, reason)`, oldest first."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT o.provider_order_id, a.amount_minor, a.reason FROM credit_adjustments a "
                "JOIN orders o ON o.id = a.order_id WHERE a.team_id = ? ORDER BY a.created_at",
                (team_id,),
            ).fetchall()
        return [(str(key), int(amount), str(reason)) for key, amount, reason in rows]

    @staticmethod
    def merge_snapshot(
        db: sqlite3.Connection, provider_order_id: str, snapshot: DepositView
    ) -> DepositView:
        """Merges `snapshot` into the deposit's stored view and returns the result."""
        row = db.execute(
            "SELECT status, amount_refunded_minor, amount_reversed_minor FROM deposit_snapshots "
            "WHERE provider_order_id = ?",
            (provider_order_id,),
        ).fetchone()
        merged = snapshot if row is None else DepositView(row[0], row[1], row[2]).merge(snapshot)
        db.execute(
            "INSERT INTO deposit_snapshots (provider_order_id, status, amount_refunded_minor, "
            "amount_reversed_minor) VALUES (?, ?, ?, ?) ON CONFLICT (provider_order_id) DO UPDATE "
            "SET status = excluded.status, amount_refunded_minor = excluded.amount_refunded_minor, "
            "amount_reversed_minor = excluded.amount_reversed_minor",
            (provider_order_id, merged.status, merged.amount_refunded, merged.amount_reversed),
        )
        return merged

    @staticmethod
    def order_amounts(db: sqlite3.Connection, order_id: str) -> tuple[int, int]:
        """An order's credit and what it nets to after its adjustments, in cents."""
        row = db.execute(
            "SELECT (SELECT COALESCE(SUM(amount_minor), 0) FROM credit_transactions "
            "WHERE order_id = ?), (SELECT COALESCE(SUM(amount_minor), 0) FROM credit_adjustments "
            "WHERE order_id = ?)",
            (order_id, order_id),
        ).fetchone()
        return int(row[0]), int(row[0]) + int(row[1])

    @staticmethod
    def order_bonus(db: sqlite3.Connection, order_id: str) -> int:
        """An order's current bonus, in cents: its grant less its claw-backs."""
        row = db.execute(
            "SELECT COALESCE(SUM(amount_minor), 0) FROM bonus_credits WHERE order_id = ?",
            (order_id,),
        ).fetchone()
        return int(row[0])

    def orders_for(self, team_id: str) -> list[dict[str, Any]]:
        """The workspace's crypto top-up orders: `accepted` (credited) or `held` (refused)."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT provider_order_id, status, reason FROM orders "
                "WHERE team_id = ? AND order_flow_code = ? ORDER BY created_at",
                (team_id, ORDER_FLOW_CODE),
            ).fetchall()
        return [
            {"provider_order_id": key, "status": status, "reason": reason}
            for key, status, reason in rows
        ]

    @staticmethod
    def stored_body(db: sqlite3.Connection, webhook_id: str) -> bytes | None:
        """The raw body of the delivery recorded as `webhook_id`: `None` when there is none, and
        empty for an event recorded before the inbox kept bodies."""
        row = db.execute("SELECT body FROM webhook_events WHERE id = ?", (webhook_id,)).fetchone()
        return None if row is None else bytes(row[0] or b"")

    def record_delivery(
        self,
        db: sqlite3.Connection,
        delivery: Delivery,
        event_type: str,
        data: Mapping[str, Any],
        state: str,
    ) -> None:
        """Adds a verified delivery to the inbox, in `db`'s transaction with its ledger effect."""
        now = time.time()
        db.execute(
            "INSERT INTO webhook_events (id, type, data, received_at, body, webhook_timestamp, "
            "webhook_signature, state, processed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                delivery.webhook_id,
                event_type,
                json.dumps(data, sort_keys=True),
                now,
                delivery.body,
                delivery.webhook_timestamp,
                delivery.webhook_signature,
                state,
                now,
            ),
        )
        self.events_changed.notify_all()

    def deliveries(self, event_types: Sequence[str]) -> list[Delivery]:
        """The inbox's deliveries of `event_types` with their raw evidence, oldest first; events
        recorded before the inbox kept it are left out."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT id, webhook_timestamp, webhook_signature, body FROM webhook_events "
                "WHERE body IS NOT NULL AND type IN (SELECT value FROM json_each(?)) "
                "ORDER BY received_at, id",
                (json.dumps(list(event_types)),),
            ).fetchall()
        return [Delivery(row[0], row[1], row[2], bytes(row[3])) for row in rows]

    def events_without_evidence(self, event_types: Sequence[str]) -> int:
        """How many stored events of `event_types` predate the inbox, so have no raw delivery."""
        with self._lock:
            row = self._connection.execute(
                "SELECT count(*) FROM webhook_events "
                "WHERE body IS NULL AND type IN (SELECT value FROM json_each(?))",
                (json.dumps(list(event_types)),),
            ).fetchone()
        return int(row[0])

    def events(self, event_type: str) -> list[dict[str, Any]]:
        with self._lock:
            rows = self._connection.execute(
                "SELECT data FROM webhook_events WHERE type = ? ORDER BY received_at", (event_type,)
            ).fetchall()
        return [json.loads(row[0]) for row in rows]

    def all_events(self) -> list[dict[str, Any]]:
        """Every stored webhook event as `{"type", "data"}`, oldest first."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT type, data FROM webhook_events ORDER BY received_at"
            ).fetchall()
        return [{"type": row[0], "data": json.loads(row[1])} for row in rows]

    def wait_for_event(
        self, event_type: str, matches: Callable[[dict[str, Any]], bool], timeout: float
    ) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        with self.events_changed:
            while True:
                for event in self.events(event_type):
                    if matches(event):
                        return event
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"no matching {event_type} webhook within {timeout:.0f}s")
                self.events_changed.wait(remaining)

    @staticmethod
    def _find_order(db: sqlite3.Connection, provider_order_id: str) -> StoredOrder | None:
        row = db.execute(
            "SELECT provider_order_id, team_id, payload, status, reason, credit_transaction_id, id "
            "FROM orders WHERE order_flow_code = ? AND provider_order_id = ?",
            (ORDER_FLOW_CODE, provider_order_id),
        ).fetchone()
        if row is None:
            return None
        return StoredOrder(row[0], row[1], json.loads(row[2]), row[3], row[4], row[5], row[6])
