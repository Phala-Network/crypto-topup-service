# Flush reverted or bisected exercise

Date: 2026-09-22.

The read-only snapshot returned `flushes=0` and `flush_exclusions=0`. Runtime logs showed the local
chain failure path every maintenance interval:

```text
flush maintenance failed
chain operation failed: error sending request for url (http://127.0.0.1:1/)
```

Revert receipt and bisection require an EVM fixture, which is not part of `deploy/local`; the route
flush pause/resume path returned HTTP 200.
