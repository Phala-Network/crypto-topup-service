.PHONY: build test lint image up down smoke infra-smoke service-smoke verify-image restore-drill

build:
	cargo build --workspace --locked

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

restore-drill:
	deploy/local/restore-drill.sh
