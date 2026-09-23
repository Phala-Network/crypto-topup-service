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

# Archive flags are appended after user arguments, so they always win. TOPUP_WAL_ARCHIVE=off is
# for restore drills: a drill instance must never archive into the production WAL prefix.
case "${TOPUP_WAL_ARCHIVE:-on}" in
    on)
        set -- "$@" \
            -c archive_mode=on \
            -c archive_timeout=60 \
            -c "archive_command=walg-cron wal-push %p"
        ;;
    off)
        echo "TOPUP_WAL_ARCHIVE=off: WAL archiving is disabled for this instance" >&2
        set -- "$@" -c archive_mode=off
        ;;
    *)
        echo "TOPUP_WAL_ARCHIVE must be on or off" >&2
        exit 64
        ;;
esac

exec /usr/local/bin/docker-entrypoint.sh "$@"
