"""The product ledger: SQLite standing in for Phala Cloud's database."""

from __future__ import annotations

import json
import sqlite3
import threading
import time
from collections.abc import Callable, Iterator, Mapping
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
CREATE TABLE IF NOT EXISTS webhook_events (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    data TEXT NOT NULL,
    received_at REAL NOT NULL
);
"""


@dataclass(frozen=True)
class StoredOrder:
    provider_order_id: str
    team_id: str | None
    payload: dict[str, Any]
    status: str
    reason: str | None
    credit_transaction_id: str | None


class ProductLedger:
    """SQLite stand-in for the product database; every write is one serialized transaction."""

    def __init__(self, path: str = ":memory:") -> None:
        self._connection = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        self._connection.execute("PRAGMA foreign_keys = ON")
        self._connection.executescript(SCHEMA)
        self._lock = threading.RLock()
        self.events_changed = threading.Condition(self._lock)

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

    def record_quote_address(self, address: str, team_id: str, quote_id: str) -> None:
        """Records a quote's address for the workspace (history only: credits name the account)."""
        with self.transaction() as db:
            db.execute(
                "INSERT OR IGNORE INTO team_addresses (address, team_id, kind, version, lock_ref) "
                "VALUES (?, ?, 'lock', NULL, ?)",
                (address.lower(), team_id, quote_id),
            )

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

    def record_event(self, event_id: str, event_type: str, data: Mapping[str, Any]) -> bool:
        """Stores a webhook once; returns False for a duplicate delivery."""
        with self.transaction() as db:
            inserted = db.execute(
                "INSERT OR IGNORE INTO webhook_events (id, type, data, received_at) "
                "VALUES (?, ?, ?, ?)",
                (event_id, event_type, json.dumps(data, sort_keys=True), time.time()),
            ).rowcount
            self.events_changed.notify_all()
        return inserted == 1

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
            "SELECT provider_order_id, team_id, payload, status, reason, credit_transaction_id "
            "FROM orders WHERE order_flow_code = ? AND provider_order_id = ?",
            (ORDER_FLOW_CODE, provider_order_id),
        ).fetchone()
        if row is None:
            return None
        return StoredOrder(row[0], row[1], json.loads(row[2]), row[3], row[4], row[5])
