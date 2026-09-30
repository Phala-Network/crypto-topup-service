#!/bin/sh
set -eu

if [ "$#" -eq 0 ]; then
    set -- postgres
fi

# The fixed path topup reads; the variable is a seam for deploy/tests/walg-cron.sh only.
marker=${TOPUP_BACKUP_TIMESTAMP_FILE:-/run/topup-observability/last-backup-unix-seconds}
marker_dir=$(dirname "$marker")
mkdir -p "$marker_dir"
if [ "$(id -u)" -eq 0 ]; then
    chown postgres:postgres "$marker_dir"
fi

# The object store, checked before PostgreSQL or a WAL-G job starts (in the Phala Cloud template its
# settings come from the deploy form, outside the attestation): WAL-G's file backend
# (WALG_FILE_PREFIX, the tests), or S3 with all three settings well formed. The endpoint is an
# https origin; only the local stacks' overlays, applied after deploy/render.sh and so never in an
# attested compose, set TOPUP_OBJECT_STORE_ALLOW_HTTP=on for their plain-http store.
refuse() {
    echo "$*" >&2
    exit 64
}
# matches VALUE ERE: VALUE is one line that ERE matches whole.
matches() {
    case "$1" in *'
'*) return 1 ;; esac
    printf '%s\n' "$1" | grep -Eqx "$2"
}
label='[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?'
check_object_store() {
    if [ -n "${WALG_FILE_PREFIX:-}" ]; then
        [ -z "${WALG_S3_PREFIX:-}" ] || refuse "set WALG_FILE_PREFIX or WALG_S3_PREFIX, not both"
        return 0
    fi
    prefix=${WALG_S3_PREFIX:-}
    bucket=${prefix#s3://}
    bucket=${bucket%%/*}
    matches "$prefix" "s3://$label(\\.$label)*(/[A-Za-z0-9._~/-]*)?" &&
        [ "${#bucket}" -ge 3 ] && [ "${#bucket}" -le 63 ] ||
        refuse "WALG_S3_PREFIX must be s3://BUCKET[/PATH] with a valid bucket name"
    scheme=https
    [ "${TOPUP_OBJECT_STORE_ALLOW_HTTP:-off}" != on ] || scheme='https?'
    endpoint=${AWS_ENDPOINT:-} port=443
    matches "$endpoint" "$scheme://$label(\\.$label)*(:[0-9]{1,5})?/?" ||
        refuse "AWS_ENDPOINT must be an https origin, https://HOST[:PORT]"
    authority=${endpoint#*://}
    authority=${authority%/}
    case "$authority" in *:*) port=${authority##*:} ;; esac
    [ "$port" -ge 1 ] && [ "$port" -le 65535 ] || refuse "AWS_ENDPOINT's port must be 1 to 65535"
    matches "${AWS_REGION:-}" '[a-z0-9]+(-[a-z0-9]+)*' ||
        refuse "AWS_REGION must be a region name such as auto or us-east-1"
}

case "$1" in
    postgres) check_object_store ;;
    -*) set -- postgres "$@"; check_object_store ;;
    walg-cron) check_object_store; exec /usr/local/bin/docker-entrypoint.sh "$@" ;;
    *) exec /usr/local/bin/docker-entrypoint.sh "$@" ;;
esac

restore=${TOPUP_RESTORE_FROM_BACKUP:-off}
case "$restore" in
    on|off) ;;
    *)
        echo "TOPUP_RESTORE_FROM_BACKUP must be on or off" >&2
        exit 64
        ;;
esac

as_postgres() {
    if [ "$(id -u)" -eq 0 ]; then
        gosu postgres "$@"
    else
        "$@"
    fi
}

# Bootstrap (deploy/RESTORE.md): an empty data directory is filled with the newest base backup in
# the backup prefix, then recovers through every archived WAL segment and promotes. Only a listing
# that succeeds and is empty lets docker-entrypoint.sh initialize a new cluster; any listing error
# (storage unreachable, credentials not sealed yet) stops here, because a new cluster archiving
# into a prefix that holds a timeline would fork it. The base backup is fetched beside PGDATA and
# moved into place only when complete, so an interrupted fetch starts over, and a data directory
# that holds anything is never touched.
bootstrap() {
    # shellcheck disable=SC2015  # either failure refuses
    backups=$(as_postgres "${WALG_BIN:-wal-g}" backup-list --json) &&
        count=$(printf '%s\n' "$backups" | jq -er 'if type == "array" then length else error end') || {
        echo "the backup prefix could not be listed; refusing to initialize an empty data directory" >&2
        exit 1
    }
    if [ "$count" -eq 0 ]; then
        if [ "$restore" = on ]; then
            echo "TOPUP_RESTORE_FROM_BACKUP=on: the backup prefix holds no base backup" >&2
            exit 1
        fi
        echo "the backup prefix holds no base backup; initializing a new cluster" >&2
        return 0
    fi
    backup_name=$(printf '%s\n' "$backups" |
        jq -er 'max_by(.time | sub("[.][0-9]+"; "") | fromdateiso8601) | .backup_name') || {
        echo "the base backup listing is invalid" >&2
        exit 1
    }
    echo "restoring base backup $backup_name" >&2
    staging="$(dirname "$PGDATA")/restore-from-backup.partial"
    rm -rf "$staging"
    as_postgres "${WALG_BIN:-wal-g}" backup-fetch "$staging" "$backup_name"
    as_postgres test -s "$staging/PG_VERSION"
    as_postgres touch "$staging/recovery.signal"
    as_postgres chmod 0700 "$staging"
    if [ -d "$PGDATA" ]; then
        rmdir "$PGDATA"
    fi
    mv "$staging" "$PGDATA"
}

if [ -z "$(ls -A "$PGDATA" 2>/dev/null)" ]; then
    bootstrap
fi
# On every start, so an interrupted recovery resumes; PostgreSQL uses it only in recovery.
set -- "$@" -c "restore_command=walg-restore-command %f %p"

# Archive flags are appended after user arguments, so they always win. The restore-check variant
# must never archive into the prefix it restores from (deploy/RESTORE.md).
if [ "$restore" = off ]; then
    set -- "$@" \
        -c archive_mode=on \
        -c archive_timeout=60 \
        -c "archive_command=walg-cron wal-push %p"
else
    echo "WAL archiving is disabled while TOPUP_RESTORE_FROM_BACKUP=on" >&2
    set -- "$@" -c archive_mode=off
fi

exec /usr/local/bin/docker-entrypoint.sh "$@"
