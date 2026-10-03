# RPC load balancing and failover

Status: proposed for 0.7.0; design only. Base: 0.6.0.

## Decision

Use **eRPC 0.3.0**, Apache-2.0, as two private sidecars, one for group A and one for
B. Generate their configurations from the attested topup configuration. Keep credit
agreement and monotonic-head enforcement in topup. Do not implement a routing engine.

Today each chain lists provider ids; topup consumes positions 0 and 1 only, although
configuration accepts more. Each id resolves to one attested URL and optionally a sealed
`TOPUP_RPC_<ID>_KEY`. A scans logs, heads and reconciliation; B independently confirms
receipts, heads and calls. An outage stalls work. See [RPC providers](../../deploy/README.md#rpc-providers),
[usage accounting](../../deploy/README.md#measuring-rpc-usage), architecture
[§7](../architecture.md#7-states-and-pump), [§8](../architecture.md#8-chain-valuation-screening), and
[§13](../architecture.md#13-reconciliation), and
[configuration](../../crates/topup/src/config.rs), [routes](../../crates/topup/src/routes.rs),
[URL/key handling](../../crates/topup/src/rpc_provider.rs), and
[EVM transport](../../crates/adapters/src/chain/evm/mod.rs).

| Choice | Verified behavior | Tradeoff |
|---|---|---|
| Alloy 2.5.0 `FallbackLayer` + `RetryBackoffLayer` | Ordinary reads race up to `active_transport_count` transports; only sync send methods are sequential by default. Fallback accepts `Ok(ResponsePacket)`, including JSON-RPC errors, and does no consensus. Retry inspects response errors and applies a retry policy. Sequential fallback visits only the top active count: setting it to 1 does not visit all backups. | Already in the Rust TCB; simplest deployment. Layer ordering can retry RPC errors, but neither layer supplies company isolation, monotonic heads, circuit breakers, sticky priorities or configurable health/rate policies. Implementing those becomes our routing product. |
| eRPC 0.3.0 | Network retries select other upstreams; per-upstream breakers; bounded hedging; programmable selection, lag tracking, integrity checks, budgets and Prometheus metrics. | Adds Go, its dependencies and a JS policy interpreter to the TCB, two processes and another release to operate. Pre-1.0 schema/defaults need pinned contract tests. Much closer to HAProxy-style routing policies; avoids maintaining a bespoke scheduler. |

Accept the TCB and pre-1.0 costs for policy configurability. Start with a small generated
policy surface, disabled hedging/cache, and explicit defaults. eRPC is adaptive ranked
routing, not a promise of HAProxy's exact round-robin/least-connections algorithms. Its
consensus feature is **not** the application's independent A/B agreement. Two containers
isolate health/configuration and keys; they do not defeat a common eRPC implementation bug.

## Verified dependency contract

Use the official [documentation](https://docs.erpc.cloud), but resolve version-sensitive
claims against documentation and source shipped at
[0.3.0](https://github.com/erpc/erpc/releases/tag/0.3.0), commit
`a98914408d5e13e848d4baba0ea06d20d58c62e9`; do not copy current-site defaults blindly.
Alloy is the repository's exact 2.5.0 dependency.

| Capability | Release documentation/source and implications |
|---|---|
| Alloy fallback/retry | [Fallback](https://docs.rs/alloy-transport/2.5.0/src/alloy_transport/layers/fallback.rs.html), [retry](https://docs.rs/alloy-transport/2.5.0/src/alloy_transport/layers/retry.rs.html): retry consumes JSON-RPC errors, but fallback alone treats them as successful packets; neither validates agreement or monotonicity. |
| Failover/retry/breaker/hedge | [Failsafe docs](https://github.com/erpc/erpc/tree/0.3.0/docs/pages/config/failsafe), [network executor](https://github.com/erpc/erpc/blob/0.3.0/erpc/network_executor.go), [upstream executor](https://github.com/erpc/erpc/blob/0.3.0/upstream/upstream_executor.go): network retry rounds can sweep several upstreams. `maxAttempts` is rounds, not a bound on HTTP sends. Breakers belong at upstream scope; explicitly set both threshold counts/capacities rather than relying on defaults. Hedging creates extra billable attempts and cancellation cannot undo sends. |
| Selection/lag/integrity | [Selection](https://github.com/erpc/erpc/blob/0.3.0/docs/pages/config/projects/selection-policies.mdx), [default policy](https://github.com/erpc/erpc/blob/0.3.0/internal/policy/default_policy.js), [integrity](https://github.com/erpc/erpc/blob/0.3.0/docs/pages/config/failsafe/integrity.mdx): default `whenEmpty(() => upstreams)` restores excluded candidates; omit it. Lag uses the corroborated (second-highest) head, so two-node and all-stale pools need application guards. `enforceHighestBlock` can synthesize `eth_blockNumber`; disable it. Intrinsic checks are useful but not consensus; authoritative checks add reads and may skip nonstandard chain encodings. |
| Rate limits | [Budgets](https://github.com/erpc/erpc/blob/0.3.0/docs/pages/config/rate-limiters.mdx): auth/project/network/upstream scopes, method rules, memory or Redis. Use memory budgets; Redis has fail-open paths. Shared keys across chains share one budget within their sidecar. Disable the implicitly enabled upstream auto-tuner. |
| Projects and secrets | [Projects](https://github.com/erpc/erpc/blob/0.3.0/docs/pages/config/projects.mdx), [config loader](https://github.com/erpc/erpc/blob/0.3.0/common/config.go): multiple projects have separate upstream/health state; endpoints are `/<project>/evm/<chainId>`. YAML uses `os.ExpandEnv`, so `${NAME}` works and an absent name expands to empty. Preflight must reject missing keys before eRPC starts. |
| Cache | [Cache docs](https://github.com/erpc/erpc/blob/0.3.0/docs/pages/config/database/evm-json-rpc-cache.mdx), [defaults](https://github.com/erpc/erpc/blob/0.3.0/common/defaults.go): `database.evmJsonRpcCache: null` disables it. Policies support explicit `finality: finalized`; absence of that field means finalized, not all states. Choose disabled cache for 0.7.0; a future cache must also be separated by group and exclude heads, pending receipts, calls at latest and unknown finality. |
| Metrics | [Definitions](https://github.com/erpc/erpc/blob/0.3.0/telemetry/metrics.go), [send path](https://github.com/erpc/erpc/blob/0.3.0/upstream/upstream.go), [poller](https://github.com/erpc/erpc/blob/0.3.0/architecture/evm/evm_state_poller.go): `erpc_upstream_request_total` counts each send attempt, including hedges; its `category` receives the method. Poller reads pass through upstream forwarding. Health-tracker counts exclude hedges and must not be used for billing. |
| Image/build/license | [Dockerfile](https://github.com/erpc/erpc/blob/0.3.0/Dockerfile), [release workflow](https://github.com/erpc/erpc/blob/0.3.0/.github/workflows/release.yml), [license](https://github.com/erpc/erpc/blob/0.3.0/LICENSE): official nonroot distroless `ghcr.io/erpc/erpc:0.3.0`, amd64/arm64, build provenance and binary SBOM/checksums. Base images and dependency lockfiles are pinned, but `npm install -g pnpm` is unversioned and Go build inputs are not demonstrated bit-for-bit reproducible. Digest pinning fixes deployed bytes; it does not prove reproducible source builds. |

Registry read-only verification on 2026-10-02 resolved the release image index to
`ghcr.io/erpc/erpc@sha256:f273ab9a061dfea908e59444093fc235308fb37cc0c4d21b3e0e6eb92ef5ae7e`.
Its linux/amd64 manifest is
`sha256:1096d910aa36f6ee8929d6665dddefe97f2c9983b57b657b6d5c3fd2c341ee39`.
Pin the platform and digest in the release manifest, verify provenance against the source
commit, record SBOM/license review and scan results. Rebuild validation must pin pnpm and
all toolchains and compare binary/image outputs; do not claim reproducibility until it passes.

## Configuration and routing

Proposed public schema below is **topup's schema, not verbatim eRPC YAML**. Group ids
serve one chain; all routes on that chain name the same groups. The full document also
contains the other chain/group definitions and routes.

```yaml
rpc_groups:
  sepolia-a:
    chain_id: 11155111
    upstreams:
      - id: tenderly-sepolia
        url: https://sepolia.gateway.tenderly.co
        sealed_key: null
        priority: 0
      - id: alchemy-sepolia
        url: https://eth-sepolia.g.alchemy.com/v2/{key}
        sealed_key: TOPUP_RPC_ALCHEMY_SEPOLIA_KEY
        priority: 1
        rate_limit: { budget: alchemy-account, max_count: 20, period: 1s }
    policy:
      selection: priority
      request_timeout: 3s
      total_timeout: 10s
      retry: { max_attempts: 2, delay: 100ms, backoff_factor: 2, max_delay: 1s, jitter: 100ms }
      circuit_breaker: { failures: 5, window: 10, half_open_after: 30s, successes: 2, success_window: 2 }
      hedge: { max_count: 0, delay: 500ms }
      health: { poll_interval: 12s, max_head_lag_blocks: 4, max_finalized_lag_blocks: 32 }
      integrity: intrinsic
  sepolia-b:
    chain_id: 11155111
    upstreams:
      - id: publicnode-sepolia
        url: https://ethereum-sepolia-rpc.publicnode.com
        sealed_key: null
        priority: 0
    policy: # same explicit policy fields as A; defaults resolved by config show
      selection: fastest
chain:
  rpc_groups: { a: sepolia-a, b: sepolia-b } # inside every route's chain section
```

Allow 1–8 upstreams per group, including multiple URLs or credentials at the same company.
`selection` is `priority` (lowest priority first, latency within a tier) or `fastest`
(adaptive latency ranking; traffic shifts as load changes). Compile these to an attested
`selectionPolicy.evalFunc`, evaluated every 1s: remove cordoned candidates, exclude lagging
nodes and high-error nodes after a minimum of 10 samples, rank eligible nodes, and hold the
primary with 30% hysteresis for 30s. No empty-pool fail-open, raw JS supplied by users,
provider auto-discovery, shared upstream repository, shadow traffic, or cross-group fallback.
Exclude unknown poller health until an initial successful probe; an empty group returns an error.
Use chain-specific poll cadence and lag thresholds; the example values are starting points.

Render A and B as separate projects on separate sidecars; all A chain groups can share
`rpc-a`, all B groups `rpc-b`. Topup derives two shared clients per chain at
`http://rpc-a:4000/sepolia-a/evm/11155111` and
`http://rpc-b:4000/sepolia-b/evm/11155111`. Restore checks use the same paths.
Generate native `failsafe` entries: timeout/retry/hedge at network scope, timeout/breaker
and `retry.maxAttempts: 1` at upstream scope. Map breaker fields to
`failureThresholdCount`, `failureThresholdCapacity`, `halfOpenAfter`,
`successThresholdCount`, `successThresholdCapacity`. Network retries are bounded by the
10s deadline, candidate count and budgets; topup retains its outer deadline/backoff without
an additional Alloy retry layer. Hedging is disabled for heads and transaction submission;
optional hedging is limited to one extra idempotent read. Do not retry execution reverts or
invalid parameters; transport failures, 429 and normalized transient RPC errors can fail over.
A legitimate null receipt or empty log set remains valid evidence for existing A/B checks.

Disable eRPC consensus, multiplexing and cache; set `directiveDefaults.enforceHighestBlock: false`,
retain non-null tagged-block and log-range checks, use `integrity.level: intrinsic`, and
reject caller directive overrides (`allowClientDirectives: ""`). No proxy-created head
is acceptable credit evidence. Reorg/receipt disagreement remains topup's responsibility.

## Safety, attestation and preflight

**Company independence.** Before rendering, expand each chain's A/B groups and require
disjoint company identities: normalized hosts plus registrable domains from a pinned Public
Suffix List (including private suffixes). Different ports, subdomains, paths or API keys
never establish independence. Reject IP literals and unrecognized suffixes in attested
production configurations; local fixtures use explicitly test-only identities. A reviewed,
attested alias table unifies company-owned domains such as PublicNode's aliases; custom
CNAMEs must resolve to a reviewed company mapping or fail validation. Domain checks cannot
prove corporate ownership, reseller independence or different backend infrastructure;
operator review is required for new companies. Never put Tenderly in both groups as fallback.

**Monotonic heads.** Extend the existing `FinalizedGuard` to serialized, shared
`(chain, group, tag)` guards for `latest`, `safe`, `finalized`. A lower response returns a
retryable stale-head error; never clamp it to the previous number or publish it. Keep guards
shared across scanners, credit checks, finality watch and reconciliation. A head reaching
the guard is an actual successful response from that group. Persist accepted watermarks
before publishing advances; initialize after restart/restore to those and committed scanner/
reconciliation cursors. A finalized hash change at an accepted height freezes the chain;
latest/safe same-height hash changes continue through existing reorg handling. A real
height regression can delay progress until recovery; safety takes precedence over availability.
One group unavailable or disagreeing always means wait, never credit from the other group.

**Attested deployment.** Add digest-pinned `rpc-a`/`rpc-b` to service, template, local and
restore-check compose variants. Extend renderer, image manifest, exact service allowlists,
`compose-policy.jq`, sealed-name validation and attested-compose tests. Inline public eRPC
YAML under content-digest config names, mounted read-only at `/erpc.yaml`; invoke only
`/erpc-server --config /erpc.yaml --require-config`. Restrict topup RPC HTTP destinations
to these private service paths; upstream HTTPS remains mandatory. No published RPC/metrics/
admin/pprof ports, Redis or persistent proxy volumes. Use read-only rootfs, dropped
capabilities, bounded CPU/memory (`GOMEMLIMIT` below the memory limit), restart policy and
private networks; sidecars receive neither DB credentials nor the dstack socket.
The attested compose hash changes for images, configs, policies and sealed-name interfaces.

**Sealed keys.** Reuse URL validation: at most one `{key}`, a whole path segment or query
value, no userinfo/fragment/host placeholder; keys retain the existing character/length
limits. Render `{key}` to `${TOPUP_RPC_ALCHEMY_SEPOLIA_KEY}` in eRPC YAML, escaping `$`
as `$$` while composing so Compose preserves the literal for eRPC's runtime expansion.
Pass each sealed name through the existing dstack sealed-env interface only to its owning
sidecar; topup and restore-check do not receive upstream keys. Missing/empty/stray keys and
normalized-name collisions fail secret preflight. Never render expanded URLs to disk or
attested config. Use `LOG_LEVEL=disabled`, `logLevel: disabled`, Docker logging driver
`none`, no tracing/config-dump/admin exposure; topup discards proxy error messages/data and reports only group/upstream ids and
allowlisted error codes/classes (it cannot redact keys it no longer receives). Native endpoint redaction is not a guarantee against echoed RPC
errors. Secret-canary tests of startup failures, responses and logs are a release gate;
if suppression leaks, fix the logging boundary before shipping. Debug only with keyless
fixtures; never enable verbose logs on keyed sidecars.

**Checks.** Offline `topup config check` rejects legacy/unknown fields, missing/unused groups,
empty lists, duplicate ids/URLs, invalid budgets/timeouts, inconsistent chain definitions,
A/B company overlap and extra roles. `--secrets` validates the sealed-key interface without
printing values. Render checks compare the two native configs to the canonical inputs and
parse them with pinned eRPC using dummy keys, never dump resolved real secrets.
Online preflight probes **every upstream directly** in its owning group: TLS, `eth_chainId`,
latest/safe/finalized and route factory/token/oracle/Multicall3 checks. Every A candidate must
support 2,000-block logs and recipient-filtered logs without a contract address. Probe proxy
paths too; require at least one eligible member in each group at runtime. A new release's
preflight requires every candidate to pass; degraded existing deployments continue waiting/
using eligible members rather than crediting with one group. Check health with actual RPC
probes, not merely eRPC process liveness; distroless provides no shell/curl healthcheck.

## Usage, alerts and runbook

Preserve admin-signed `/v1/admin/metrics` and the existing cost formula. Export
`topup_rpc_calls_total{provider,chain_id,method}` from summed eRPC **upstream** attempt
counters, mapping `upstream` to the configured upstream id, `network` to chain id and
`category` to the existing 16 methods/`other`. Include errors, canceled sent hedges,
retries, splits, health polls and integrity/preflight sends. Do not count the internal
HTTP hop as another provider call. Direct runtime preflight probes retain the existing
per-upstream CountingLayer and are added once to the eRPC totals; standalone CLI preflight
reports its own process usage. Rename internal-hop counters to logical group requests; retain
old upstream ids during migration. Fetch metrics privately, validate bounded labels and
export raw counters without silently filling gaps with zero; expose sidecar-specific
start times and scrape freshness so restarts are distinguishable from usage decreases.
Report unavailable usage if a scrape fails. Rate/delta calculations handle counter resets;
extra health traffic must appear in the staging cost comparison.

Expose group eligibility, accepted heads, regression counters and scrape health through
the same admin surface. eRPC supplies upstream attempts/errors/latency, lag, breaker
transitions, selection switches, rate limiting and integrity violations. Page on zero
eligible A or B for 1 minute, scanner/finality/reconciliation stalled beyond existing
chain SLOs, finalized hash conflict, or missing usage telemetry for 5 minutes. Warn on
sustained lag/regressions, frequent switches/breaker trips, 429s or unexpected cost growth.
A healthy proxy process does not clear a group-outage alert.

Runbook: identify chain/group and last accepted heads; inspect sanitized metrics and
keyless capability probes. Check outage versus lag, quota, key expiry or wrong chain.
Drain a failing upstream through an attested config PR/upgrade, or rotate an existing
sealed key through the owner workflow; adding a name requires resealing its interface.
Never move a company across groups to restore availability or lower a head watermark.
If a whole group is lost, leave credits pending and communicate the delay. Restore an
independent member, verify both groups, resume from committed cursors and reconcile for
missed deposits/duplicate credits. Roll back the complete image/config release only with
compatible watermarks; preserve sealed material and DB cursors. No production operation
is authorized by this design PR.

## Validation and 0.7.0 migration

- Unit tests: exactly A/B roles; legacy lists of 0/1/3+ ids rejected; host/registrable-domain
  and alias overlap; different keys at one company; URL/key validation and sealed-name
  collisions; deterministic render and literal env escaping; invalid policy bounds;
  concurrent tag guards, reorgs, persistence/restart and cursor floors; metric mapping,
  extra attempts, reset/scrape-failure behavior and secret redaction.
- Local integration: run the pinned sidecars with deterministic JSON-RPC fixtures, separate
  A/B company identities and one healthy member per group. Add a failing connection/timeout
  upstream, a lagging upstream (including the observed 156-block finalized regression),
  and an HTTP-200 JSON-RPC-error upstream (`-32005`/429 plus nonretryable revert/invalid
  params). Assert bounded failover, lag exclusion/recovery, breaker open/half-open recovery,
  no published head regression, no synthetic head, correct error classification and counts.
  Kill all A, then all B: no credit until both return and agree. Exercise divergent receipts,
  reorgs, restart/restore, all-stale pools, multiple keys/quota sharing, disabled cache,
  optional bounded read hedging and canary secrets echoed in errors. Compare fixture wire
  sends to counters, including internal poller sends; an accounting mismatch blocks release.
- Compose/attestation tests: image/config/policy/name changes alter the attested hash;
  secret rotation does not alter public config; no plaintext key in compose, errors or logs;
  service/socket/port/credential allowlists and preflight cover both sidecars and restore.

Staging currently uses keyless Tenderly A and PublicNode B on Sepolia and Base Sepolia.
First map the existing four ids into singleton groups, preserving their company separation,
route versions and cursors; this stage still has no upstream redundancy. Rehearse offline/
locally, then use the normal reviewed staging release workflow in a follow-up implementation
PR. Add Alchemy only to A and Infura only to B (or other reviewed disjoint companies), with
per-chain upstream ids, capability checks, sealed keys and account-wide budgets. Multiple
URLs/keys at one vendor improve quota/endpoint resilience, not company-outage resilience.
Observe a full reconciliation/finality cycle and compare billed usage before promoting.

Breaking changes in 0.7.0: replace top-level `rpc_providers` and ordered
`chain.rpc_providers` with `rpc_groups` and explicit `{a, b}` references; remove implicit
provider defaults and reject legacy fields (including >2 ids) with a migration error.
Offer an offline migration command for **exactly two** old ids, requiring company review;
never silently discard extra ids. Require the sidecars, new attested compose/images and
role-scoped sealed-env interfaces; migrate template/self-hosting/preflight/restore docs.
API key names can remain unchanged, but their recipient services change. Usage provider
labels now identify actual upstreams; logical group traffic is separate, and proxy restarts
require per-sidecar reset handling. Head watermark persistence needs an additive migration.
Do not deploy 0.7.0 with a 0.6.0 compose/config or downgrade around its guards.
