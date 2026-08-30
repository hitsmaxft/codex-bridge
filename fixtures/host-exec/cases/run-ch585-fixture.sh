#!/bin/sh

case "${1:-}" in
    ok)
        printf 'fixture stdout\n'
        printf 'fixture stderr\n' >&2
        exit 7
        ;;
    flood)
        /usr/bin/awk 'BEGIN { for (i = 0; i < 40000; i++) printf "x" }'
        ;;
    sleep)
        /bin/sleep 5
        ;;
    *)
        printf 'unknown fixture mode\n' >&2
        exit 64
        ;;
esac
