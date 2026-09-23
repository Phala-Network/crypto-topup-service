FROM rust:1.98.1-slim-trixie@sha256:f47a8de237dcbb0b0ce1099901e60a89728e3d51f24e664b40e947171538ade7 AS builder

ARG SOURCE_DATE_EPOCH=0
ENV CARGO_INCREMENTAL=0 \
    SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}

WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .sqlx ./.sqlx
COPY crates ./crates
RUN SQLX_OFFLINE=true \
    RUSTFLAGS="--remap-path-prefix=/workspace=. -C link-arg=-Wl,--build-id=none" \
    cargo build --release --locked -p topup

FROM gcr.io/distroless/cc-debian13:latest@sha256:4594d59540d1948417f6ca2829ddd9294493a7c68b7528f4dd459de7f203a750

COPY --from=builder --chown=nonroot:nonroot /workspace/target/release/topup /usr/local/bin/topup
USER nonroot:nonroot
CMD ["topup", "--help"]
