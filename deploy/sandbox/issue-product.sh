#!/usr/bin/env bash
# Issues sandbox product credentials: registers the integrator's product slug, public key, and
# webhook URL, and writes an audit row. The integrator keeps the private seed (`topup-sdk keygen`);
# only the public key reaches the operator. The key id and settlement URL are not stored here: the
# attested route file is their only source (render-route.sh).
#
# HUMAN-ONLY on Sepolia: an operator runs this through the sandbox's administrative database
# access. PSQL selects the client command, for example the local stack's
#   PSQL="docker compose -p NAME -f ... exec -T postgres psql -U postgres -d topup"
# The product's route file must name the same slug.
set -euo pipefail
export LC_ALL=C

usage() {
    echo "usage: $0 --slug SLUG --public-key BASE64 --webhook-url URL --operator NAME" \
        "[--allow-http]" >&2
    exit 2
}

slug="" public_key="" webhook_url="" operator="" allow_http=0
while (($#)); do
    case "$1" in
        --slug) slug="${2:-}"; shift 2 ;;
        --public-key) public_key="${2:-}"; shift 2 ;;
        --webhook-url) webhook_url="${2:-}"; shift 2 ;;
        --operator) operator="${2:-}"; shift 2 ;;
        --allow-http) allow_http=1; shift ;;
        *) usage ;;
    esac
done
[[ -n "$slug" && -n "$public_key" && -n "$webhook_url" && -n "$operator" ]] || usage

[[ "$slug" =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]] || { echo "invalid slug" >&2; exit 2; }
key_bytes=$(printf '%s' "$public_key" | base64 -d 2>/dev/null | wc -c) || key_bytes=0
[[ "$key_bytes" == 32 ]] || { echo "public key must be standard base64 of 32 bytes" >&2; exit 2; }
if [[ "$webhook_url" != https://* ]] && ! [[ "$allow_http" == 1 && "$webhook_url" == http://* ]]; then
    echo "the webhook URL must use https (use --allow-http only for the local stack)" >&2
    exit 2
fi

# psql variables are quoted by psql itself (:'name'), never interpolated into SQL text.
read -r -a psql_command <<<"${PSQL:-psql}"
"${psql_command[@]}" --set=ON_ERROR_STOP=1 --quiet --tuples-only --no-align \
    --set=slug="$slug" --set=public_key="$public_key" --set=webhook_url="$webhook_url" \
    --set=actor="operator:$operator" <<'SQL'
BEGIN;
INSERT INTO products (id, slug, webhook_url, pubkey)
VALUES (gen_random_uuid(), :'slug', :'webhook_url', :'public_key');
INSERT INTO audit (id, actor, action, subject, reason)
VALUES (gen_random_uuid(), :'actor', 'product.issue', :'slug', 'sandbox credential issuance');
SELECT json_build_object('product_slug', :'slug');
COMMIT;
SQL
