# Gas refill exercise

Date: 2026-09-22.

The authenticated all-scope pause included `flush` and returned HTTP 200. The app-role snapshot
returned `flushes=0`.

The local compose exposes no EVM RPC and intentionally logs flush chain-read failures, so operator
balance/nonce and a Finance Safe transfer could not be exercised. Those steps remain human-only.
