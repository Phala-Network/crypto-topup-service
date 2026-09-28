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

# Stripe's smokescreen, the webhook egress proxy (docs/design/multi-tenant.md §8), which the
# compose runs from this image as the `smokescreen` sidecar, so it is pinned and attested with
# the service's digest (deploy/README.md, "Webhook egress"). Stripe publishes no image: this builds
# tag v0.1.0's commit with its vendored modules and the local toolchain only.
FROM golang:1.27-trixie@sha256:433790e515d27dc6003e847e644cc0af956985cf315c1c58a3b73ee2dd305183 AS smokescreen

WORKDIR /src
RUN git init -q . \
    && git fetch -q --depth 1 https://github.com/stripe/smokescreen.git \
        609eb8931420453daf5893509be0b25b21bd9edb \
    && git checkout -q FETCH_HEAD \
    && CGO_ENABLED=0 GOTOOLCHAIN=local GOFLAGS=-mod=vendor \
        go build -trimpath -buildvcs=false -ldflags='-s -w -buildid=' -o /out/smokescreen .

FROM gcr.io/distroless/cc-debian13:latest@sha256:4594d59540d1948417f6ca2829ddd9294493a7c68b7528f4dd459de7f203a750

COPY --from=builder --chown=nonroot:nonroot /workspace/target/release/topup /usr/local/bin/topup
COPY --from=smokescreen --chown=nonroot:nonroot /out/smokescreen /usr/local/bin/smokescreen
USER nonroot:nonroot
CMD ["topup", "--help"]
