# Rejected funds at treasury exercise

Date: 2026-09-22.

Status: blocked on #57 reporting plus a Compliance disposition and Finance Safe fixture.

G2 exercised once: [ ]

The read-only app-role snapshot returned `deposits=0`, `flushes=0`, and `refunds=0`. A signed
`GET /v1/admin/report/daily` returned:

```text
501 {"error":{"code":"not_implemented","message":"handler is owned by work package C12","work_package":"C12"}}
```

Treasury token balance and `Flushed` receipt checks require an EVM and were not feasible locally.
