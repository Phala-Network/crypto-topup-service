.PHONY: build build-topup test lint image up down smoke infra-smoke service-smoke verify-image \
	deploy-check alerts-check runbook-check sdk-check sdk-generate sandbox-local

build:
	cargo build --workspace --locked

# The runtime binary alone, as the image builds it, so no tool-only feature is unified into it.
build-topup:
	cargo build --release --locked -p topup

test:
	cargo test --workspace --locked

lint:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cargo deny --locked check

image:
	docker build --build-arg SOURCE_DATE_EPOCH="$${SOURCE_DATE_EPOCH:-0}" -t crypto-topup-service:dev .
	docker run --rm crypto-topup-service:dev topup --help

up:
	docker compose -f deploy/local/docker-compose.yml up --build -d postgres dstack-simulator backup

down:
	docker compose -f deploy/local/docker-compose.yml down --remove-orphans

smoke:
	deploy/local/infra-smoke.sh

infra-smoke:
	deploy/local/infra-smoke.sh

service-smoke:
	SERVICE_SMOKE=1 deploy/local/service-smoke.sh

verify-image:
	deploy/verify-image.sh

deploy-check:
	cd contracts && forge fmt --check
	cd contracts && forge build
	cd contracts && forge test
	deploy/contracts/check-build.sh --check
	deploy/contracts/test-determinism.sh

alerts-check:
	deploy/check-alerts.sh

runbook-check:
	deploy/runbooks/check.sh

sdk-check:
	$(MAKE) -C sdk/python sync check

sdk-generate:
	$(MAKE) -C sdk/python generate

sandbox-local:
	deploy/sandbox/run-local.sh
