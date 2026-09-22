# Python SDK tasks. Every target runs the tools pinned in uv.lock.
UV ?= uv
OPENAPI := ../../crates/topup/openapi.json
GENERATED := src/topup_client

.PHONY: sync generate check-generated lint test check

sync:
	$(UV) sync --locked --all-groups

generate:
	$(UV) run --locked --group codegen openapi-python-client generate \
		--path $(OPENAPI) \
		--config openapi-python-client.yaml \
		--meta none \
		--output-path $(GENERATED) \
		--overwrite \
		--fail-on-warning

# Regeneration must be a no-op: the committed client always matches the committed OpenAPI spec.
check-generated: generate
	git diff --exit-code -- $(GENERATED)
	test -z "$$(git status --porcelain --untracked-files=all -- $(GENERATED))"

lint:
	$(UV) run --locked ruff check . ../examples ../../deploy/sandbox/scenarios
	$(UV) run --locked ruff format --check . ../examples ../../deploy/sandbox/scenarios
	$(UV) run --locked mypy src/topup_sdk tests ../examples ../../deploy/sandbox/scenarios

test:
	$(UV) run --locked pytest

check: lint test check-generated
