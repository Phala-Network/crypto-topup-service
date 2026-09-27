"""Prefixed object ids: a type prefix and the 32 lowercase hex digits of a UUID, as Stripe's
`pi_…` ids are. A deposit's UUID is the deterministic `deposit_id`, so `dep_` ids can be
recomputed from chain evidence."""

from __future__ import annotations

import re
import uuid

QUOTE = "qt_"
DEPOSIT = "dep_"
REFUND = "re_"
EVENT = "evt_"

_HEX = re.compile(r"[0-9a-f]{32}")


def object_id(prefix: str, value: uuid.UUID) -> str:
    """Formats `value` as an id with `prefix`."""
    return prefix + value.hex


def parse_id(prefix: str, value: str) -> uuid.UUID:
    """Parses an id with `prefix`; raises `ValueError` for anything else."""
    hex_digits = value.removeprefix(prefix)
    if hex_digits == value or not _HEX.fullmatch(hex_digits):
        raise ValueError(f"not a {prefix} id: {value!r}")
    return uuid.UUID(hex=hex_digits)
