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
    postgres)
        set -- "$@" \
            -c archive_mode=on \
            -c archive_timeout=60 \
            -c "archive_command=walg-cron wal-push %p"
        ;;
    -*)
        set -- postgres "$@" \
            -c archive_mode=on \
            -c archive_timeout=60 \
            -c "archive_command=walg-cron wal-push %p"
        ;;
esac

exec /usr/local/bin/docker-entrypoint.sh "$@"
