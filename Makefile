.PHONY: build test lint image

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
	docker run --rm crypto-topup-service:dev --help
