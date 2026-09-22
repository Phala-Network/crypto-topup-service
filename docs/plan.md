# Delivery Plan

Companion to [architecture.md](architecture.md) (v5). No dates: work is ordered by dependency
and grouped into lanes that run in parallel. Every work package (WP) is sized for one AI agent,
one branch, one PR. Humans make the decisions listed in §1, review PRs, and press merge.

## 1. Human decisions (blocking inputs)

| Decision | Needed by | Owner |
|---|---|---|
| Treasury Safe: owners, threshold, deployed at the same address on Sepolia and mainnet | A2 | Finance |
| Two RPC providers and API keys | C3 | Ops |
| Object storage bucket and credentials for backups | D3 | Ops |
| Contract audit vendor | A3 | Security |
| Policy numbers: min/max deposit, min credit, spread, lock window, exposure caps, flush thresholds | G2 | Finance, Risk |
| Compliance determinations: region, Travel Rule applicability, KYT timing | G2 | Compliance |
| Refund policy sign-off | G2 | Finance |
| Pilot allowlist of workspaces | G2 | Product |

## 2. Rules for AI agents

- One WP per agent, on a branch named `wp/<id>-<slug>`, from `main`. Read `docs/architecture.md`
  sections cited by the WP before writing code; the spec is the source of truth. If the spec
  is wrong or ambiguous, open a PR against the spec first.
- Definition of done, every WP: code plus tests listed in the WP; `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`, and
  `forge test` (when contracts are touched) pass in CI; no `unwrap`/`expect`/float in `core`;
  no secrets in the repo; PR description states what changed, how it was verified, and which
  spec sections it implements.
- A second agent reviews every PR with the checklist in §6 before a human merges.
- Never merge to `main` directly. Never deploy to mainnet; mainnet actions are human-run from
  documented commands.
- Keep implementations simple; do not add configuration, abstractions, or features the spec
  does not ask for.

## 3. Lanes and work packages

Dependencies are listed as `after:`. WPs without `after` in a lane can start immediately.

### Lane A — Contracts (`contracts/`)

| WP | Deliverable | Tests / done when | Spec |
|---|---|---|---|
| A1 Forwarder + Factory | `Forwarder.sol` (immutable treasury and factory, `flush(token)`, ETH via `call`, SafeERC20), `ForwarderFactory.sol` (AccessControl, implementation created in constructor, `addressOf`, batch `flush`, `Flushed` event) | Foundry unit, fuzz, and invariant tests: flush can only pay the treasury; predicted address equals deployed; ETH path; reentrancy with a hook token; batch is atomic; a failing salt reverts the whole flush, documented | §4 |
| A2 Deterministic deployment | Scripts for the Arachnid proxy with plain salts; identical init code on every chain; verification script comparing factory/implementation/treasury across chains; Sepolia deployment `after: A1` and treasury Safe | Same addresses on two testnets; verification script green | §4, §14 |
| A3 Audit package | Threat model, invariants, test report, deployment procedure `after: A1, A2` | Package accepted by the audit vendor; findings tracked as WPs | §4 |

### Lane B — Core domain (`crates/core`, no I/O)

| WP | Deliverable | Tests / done when | Spec |
|---|---|---|---|
| B1 Workspace scaffold | Cargo workspace, lint policy, `cargo-deny`, CI workflow, reproducible Dockerfile skeleton, `topup` binary with `--help` | CI green on an empty workspace | §5 |
| B2 Money and address math | Newtypes, checked credit formula with 512-bit intermediate, UUIDv5 deposit id with normalization, CREATE2 address prediction, route/chain file schema `after: B1` | `proptest` monotonicity and `n − 1` bound; address math matches Foundry vectors from A1; sample route file parses | §2, §4, §6 |
| B3 State machine | `DepositState`, `StepOutcome`, `next()`, backoff schedule `after: B1` | Exhaustive transition table test; invalid transitions rejected | §7 |
| B4 Valuation rules | Freshness, ratio deviation, FX guard, stablecoin peg guard, lock price and tolerance rules `after: B2` | Table-driven tests incl. boundary values | §8, §9 |
| B5 Screening rules | Bounds, pause scopes, sanctions result mapping `after: B2` | Table-driven tests | §8, §15 |

### Lane C — Service (`crates/adapters`, `crates/topup`)

| WP | Deliverable | Tests / done when | Spec |
|---|---|---|---|
| C1 Database | `sqlx` migrations for every table in §6, repositories, transition writer with lease CAS and same-transaction outbox `after: B3` | Integration tests on Postgres: CAS rejects stale lease; append-only enforced | §6, §7 |
| C2 Pump | Claim query, lease renewal, step dispatch, retry scheduling, age alerts `after: C1` | Two pumps racing on one deposit; stale lease returning late; crash between intent and persist | §7 |
| C3 Scanner | Per-chain `finalized` scan with windowing, address batching, `ON CONFLICT DO NOTHING`, backfill for new addresses, route selection by `(chain, asset)` `after: C1` | `anvil` tests: duplicate logs, unsupported asset → rejected row, backfill, cursor advance after commit | §8 |
| C4 Confirm step | `GET`-by-key first, two-provider finality, provisional evidence correction, quote fetch (Coin Metrics, Binance, Kraken adapters), credit computation `after: C2, B4` | Provider disagreement; corrected evidence; stale/divergent/depeg prices; below minimum; restore adoption via `GET` | §7, §8, §11 |
| C5 Screen step | Sanctions oracle `eth_call` on both providers, bounds, pauses `after: C2, B5` | Sanctions hit; provider outage → retry | §8 |
| C6 Settle step | RFC 9421 signer, `Idempotency-Key`, typed outcomes, `GET` before resend, payload retention `after: C2` | Mock product: accepted, processing, rejected, `409`, `422`, timeout then `GET` | §11 |
| C7 Flusher | Planning by on-chain balance and gas ratio, batch `flush`, operator nonce lock, replacement, reverted handling, `Flushed` rows, log-position linkage; isolates persistently failing addresses by splitting the batch (bisect) and alerting `after: C1, A1` | `anvil`: flush carrying pending and rejected deposits; deposit backfilled after flush; same-block flush-then-deposit; replacement; reverted; operator rotation | §10 |
| C8 Reconciler | Every check in §13 with the two safe repairs and post-restore mode `after: C3, C6, C7` | Each check exercised with a seeded mismatch | §13 |
| C9 API | `axum` routes of §12 (accounts, addresses, rotate, rate locks, deposits, limits, pause scopes, refund requests, attestation, admin), product signature verification, tenant checks, `utoipa` OpenAPI `after: C1` | Route tests incl. cross-tenant denial; OpenAPI snapshot | §12 |
| C10 Rate locks | Creation with atomic exposure reservation and rate limit, EIP-681 URI, single consumption, tolerance, expiry event, cancel, resume `after: C4, C9` | Exact, over, under, late, double payment, cancel-then-pay | §9 |
| C11 Signer and attestation | `signer::dstack` (`operator/v1`, `settlement/v1`, `backup/v1`), zeroization, attestation endpoint with nonce, startup contract checks `after: B1` | dstack simulator tests; recorded quote verification | §10, §14 |
| C12 Refunds and support | `refunds` table flow, `deposit.refunded`, support lookup filters, `nudge`, daily report `after: C9` | Refund request → approve → record → event; report snapshot | §12, §15 |
| C13 Outbox delivery | Standard Webhooks sender, retry, delivery log, CLI replay `after: C1` | Signature verified by a reference receiver; replay by id | §12 |

### Lane D — Deployment and operations

| WP | Deliverable | Tests / done when | Spec |
|---|---|---|---|
| D1 Images | Reproducible distroless `topup` image; `postgres-walg` image; digests pinned `after: B1` | Two builds produce identical digests | §5, §14 |
| D2 dstack compose and staging | Compose with encrypted env, egress allow-list, gateway ingress; staging CVM on Sepolia `after: D1, C11` | Service attests; `topup route validate` passes in the CVM | §14 |
| D3 Backup and restore | WAL-G archiving with `archive_timeout=60`, encrypted with the backup key, `restore-check`, weekly drill job `after: D2` | Restore into a throwaway CVM passes `restore-check`; post-restore reconciliation clean | §14 |
| D4 Observability | `tracing` spans, metrics, alert rules of §16, dashboards `after: C2` | Alerts fire in staging for each rule | §16 |
| D5 Runbooks | One document per runbook in §15, each with exact commands `after: D2` | Each runbook executed once in staging | §15 |

### Lane E — Integration (monorepo and SDK)

| WP | Deliverable | Tests / done when | Spec |
|---|---|---|---|
| E1 Conformance suite | Runnable suite exercising the six product obligations and every response type `after: C6` | Passes against the mock product; fails against a deliberately broken one | §11 |
| E2 Phala Cloud settlement endpoint (monorepo) | Signature verification, order find-or-create with partial unique index, `complete_order_payment` in one transaction, `GET` by key, own caps, log verification, deposit id recomputation `after: E1` | Conformance suite green in monorepo CI; concurrency test | §11 |
| E3 Phala Cloud UI (monorepo) | Quote-first checkout, persistent address option, waiting screen, history, limits, refund request, notifications, pause and compliance messaging `after: C9, C10` | Browser verification of every row in the §12 UX checklist | §12 |
| E4 SDK and sandbox | Generated client from OpenAPI, signing helper, idempotent operations, Python example; sandbox on Sepolia with scripted scenarios `after: C9, D2` | Example runs end to end against the sandbox | §12 |
| E5 Finance and compliance procedures | Refund workflow with the Safe, daily report consumers, compliance case handling for rejected deposits `after: C12` | One refund executed in staging; report reviewed by finance | §15 |

## 4. Gates

| Gate | Passes when |
|---|---|
| G0 Foundations | A1, B1, B2, B3, C1 merged; CI green |
| G1 Sepolia end to end | A2, C2–C7, C9–C11, D2 merged; a quote-first and a persistent-address deposit credited on Sepolia through the mock product; flush confirmed; restore drill passed (D3) |
| G2 Mainnet pilot go/no-go | A3 audit closed; E2, E3, E5 merged; conformance green against Phala Cloud; all §1 decisions recorded; every runbook exercised once; §17 Phase 1 acceptance list checked by a human |
| G3 GA | Phase 2 items of §17 and the GA column of §18 delivered; caps raised by finance |

## 5. Ordering summary

```text
A1 ─┬─ A2 ─ A3
    └─ C7
B1 ─┬─ B2 ─┬─ B4 ─ C4 ─ C10
    │      └─ B5 ─ C5
    ├─ B3 ─ C1 ─┬─ C2 ─ C4/C5/C6 ─ C8
    │           ├─ C3 ─ C8
    │           ├─ C9 ─ C10/C12
    │           └─ C13
    ├─ C11 ─ D2 ─ D3/D5
    └─ D1 ─ D2
C6 ─ E1 ─ E2        C9/C10 ─ E3        C9/D2 ─ E4        C12 ─ E5
```

Parallelism at start: A1, B1 (then B2/B3 immediately after), and D1 can run at once; C1 and
C11 follow B1/B3; the C lane fans out after C1.

## 6. Review checklist (second agent, every PR)

1. Implements exactly the cited spec sections; no extra features or configuration.
2. Every money path uses the `core` newtypes; no float, no unchecked arithmetic, no `as`.
3. Every external effect writes its intent before the call and is idempotent on retry.
4. Tests listed in the WP exist and fail if the behaviour is removed.
5. No secret, key, or credential can reach a log, error, or response (type-level check).
6. Migrations are additive and reversible; append-only tables have no update or delete path.
7. PR description names the verification commands actually run and their results.

## 7. Issue breakdown

Issue #2 is closed in favour of one issue per WP, labelled by lane and gate, each linking the
spec sections and the WP row above. The monorepo receives issues for E2 and E3.
