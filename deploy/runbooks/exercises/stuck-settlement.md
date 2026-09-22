# Stuck settlement exercise

Date: 2026-09-22.

Status: blocked on the nudge implementation in #57 and a seeded mock-product settlement.

G2 exercised once: [ ]

```sh
# Signed POST /v1/admin/deposits/00000000-0000-0000-0000-000000000001/nudge
```

Observed the current `main` gap exactly:

```text
501 {"error":{"code":"not_implemented","message":"handler is owned by work package C12","work_package":"C12"}}
settlements rows: 0
```

No product settlement simulator is part of `deploy/local`, so GET-first adoption was not seeded.
