#!/bin/sh
set -eu

if [ "$#" -eq 0 ]; then
    set -- postgres
fi

marker=${TOPUP_BACKUP_TIMESTAMP_FILE:-/run/topup-observability/last-backup-unix-seconds}
marker_dir=$(dirname "$marker")
mkdir -p "$marker_dir"
if [ "$(id -u)" -eq 0 ]; then
    chown postgres:postgres "$marker_dir"
fi

case "$1" in
    postgres) ;;
    -*) set -- postgres "$@" ;;
    *) exec /usr/local/bin/docker-entrypoint.sh "$@" ;;
esac

archive=${TOPUP_WAL_ARCHIVE:-on}
case "$archive" in
    on|off) ;;
    *)
        echo "TOPUP_WAL_ARCHIVE must be on or off" >&2
        exit 64
        ;;
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

# Bootstrap from backup (deploy/RESTORE.md): an empty data directory is filled with the newest
# base backup and then recovers through every archived WAL segment and promotes. The base backup
# is fetched beside PGDATA and moved into place only when complete, so an interrupted fetch starts
# over, and a data directory that holds anything is never touched.
restore_from_backup() {
    staging="$(dirname "$PGDATA")/restore-from-backup.partial"
    rm -rf "$staging"
    backup_name=$(as_postgres "${WALG_BIN:-wal-g}" backup-list --json |
        jq -er 'max_by(.time | sub("[.][0-9]+"; "") | fromdateiso8601) | .backup_name') || {
        echo "TOPUP_RESTORE_FROM_BACKUP=on: no base backup could be listed" >&2
        exit 1
    }
    echo "TOPUP_RESTORE_FROM_BACKUP=on: restoring base backup $backup_name" >&2
    as_postgres walg-backup-fetch "$staging" "$backup_name"
    as_postgres test -s "$staging/PG_VERSION"
    as_postgres touch "$staging/recovery.signal"
    as_postgres chmod 0700 "$staging"
    if [ -d "$PGDATA" ]; then
        rmdir "$PGDATA"
    fi
    mv "$staging" "$PGDATA"
}

if [ "$restore" = on ]; then
    if [ -z "$(ls -A "$PGDATA" 2>/dev/null)" ]; then
        restore_from_backup
    else
        echo "TOPUP_RESTORE_FROM_BACKUP=on: $PGDATA is not empty and is started as is" >&2
    fi
    # On every start, so an interrupted recovery resumes; PostgreSQL uses it only in recovery.
    set -- "$@" -c "restore_command=walg-restore-command %f %p"
    # A restored instance must never archive into the WAL prefix it restores from.
    archive=off
fi

# Archive flags are appended after user arguments, so they always win. TOPUP_WAL_ARCHIVE=off is
# for restore drills: a drill instance must never archive into the production WAL prefix.
if [ "$archive" = on ]; then
    set -- "$@" \
        -c archive_mode=on \
        -c archive_timeout=60 \
        -c "archive_command=walg-cron wal-push %p"
else
    echo "WAL archiving is disabled for this instance" >&2
    set -- "$@" -c archive_mode=off
fi

exec /usr/local/bin/docker-entrypoint.sh "$@"
