# Contributing

## Setup

The committed `rust-toolchain.toml` selects Rust 1.98 with the formatting and lint components.
Install that toolchain, Foundry v1.8.3 for contract work, and cargo-deny 0.20.2 for dependency
policy checks:

```sh
rustup toolchain install 1.98 --profile minimal --component rustfmt,clippy
foundryup --install v1.8.3
cargo install cargo-deny --version 0.20.2 --locked
```

Use the package manager and committed `Cargo.lock`; do not update dependencies as a side effect
of unrelated work. The common commands are available through `make`:

```sh
make build
make test
make lint
make image
```

## Branches and pull requests

Create one branch per work package from `main`, named `wp/<id>-<slug>`, for example
`wp/b1-workspace-scaffold`. Keep the pull request scoped to that work package and cite the
implemented sections of `docs/architecture.md` and `docs/plan.md`.

## Lint policy

Every crate forbids unsafe code and warns on missing public documentation. The pure `core` crate
and the boundary-defining `adapters` crate additionally deny unchecked arithmetic, floating-point
arithmetic, `as` conversions, `unwrap`, `expect`, indexing and slicing, and panics. These rules keep
money paths explicit and make fallible behavior visible at review time.

Tests may use direct assertions and fixtures that intentionally exercise these operations. Put the
exception at the crate root so production code remains covered:

```rust
#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::expect_used,
        clippy::float_arithmetic,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )
)]
```

The `topup-core` dependency policy test reads `cargo metadata` and rejects runtime dependencies in
the initial scaffold. Future pure dependencies must be reviewed and the policy test narrowed to an
explicit I/O denylist before they are added; `tokio` and I/O frameworks never belong in `core`.

## Reproducible build baseline

Always build release artifacts with the committed lockfile. Set `SOURCE_DATE_EPOCH` to the source
commit timestamp when producing an image:

```sh
export SOURCE_DATE_EPOCH="$(git log -1 --pretty=%ct)"
cargo build --release --locked
docker build --build-arg SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" -t crypto-topup-service:dev .
docker run --rm crypto-topup-service:dev --help
```

The Debian 12 builder and distroless runtime images are pinned by digest and share the same libc
baseline. Full verification that two builds produce identical image digests is owned by work
package D1; B1 establishes the inputs and build flags only.

## Review checklist

1. Implements exactly the cited spec sections; no extra features or configuration.
2. Every money path uses the `core` newtypes; no float, unchecked arithmetic, or `as` conversion.
3. Every external effect writes its intent before the call and is idempotent on retry.
4. Tests listed in the work package exist and fail if the behavior is removed.
5. No secret, key, or credential can reach a log, error, or response.
6. Migrations are additive and reversible; append-only tables have no update or delete path.
7. The pull request names the verification commands actually run and their results.
