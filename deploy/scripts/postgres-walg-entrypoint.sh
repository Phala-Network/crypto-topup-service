#!/bin/sh
set -eu

if [ "$#" -eq 0 ]; then
    set -- postgres
fi

case "$1" in
    postgres)
        set -- "$@" \
            -c archive_mode=on \
            -c archive_timeout=60 \
            -c "archive_command=wal-g wal-push %p"
        ;;
    -*)
        set -- postgres "$@" \
            -c archive_mode=on \
            -c archive_timeout=60 \
            -c "archive_command=wal-g wal-push %p"
        ;;
esac

exec /usr/local/bin/docker-entrypoint.sh "$@"
