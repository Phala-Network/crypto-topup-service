# Lock exposure near cap exercise

Date: 2026-09-22.

```text
rate_locks rows: 0
quotes scope accepted in authenticated pause response
quotes scope removed in authenticated resume response
```

C10 is not on `main`, so exposure reservation, expiry, and cap exhaustion could not be seeded in the
local stack.
