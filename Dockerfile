FROM rust:1.98-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730 AS builder

ARG SOURCE_DATE_EPOCH=0
ENV CARGO_INCREMENTAL=0 \
    SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}

WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .sqlx ./.sqlx
COPY crates ./crates
RUN SQLX_OFFLINE=true \
    RUSTFLAGS="--remap-path-prefix=/workspace=. -C link-arg=-Wl,--build-id=none" \
    cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:latest@sha256:e5d81ddde149641e2a9ba55be4545bc125c67de07508b03ba4c22e6eb0ded5aa

COPY --from=builder --chown=nonroot:nonroot /workspace/target/release/topup /usr/local/bin/topup
USER nonroot:nonroot
CMD ["topup", "--help"]
