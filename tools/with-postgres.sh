#!/usr/bin/env bash
# Usage: with-postgres.sh FILE.sql... -- COMMAND...
# Starts a throwaway PostgreSQL cluster on a Unix socket, loads the SQL files, and runs the
# command with RUSUNA_POSTGRES set to the connection string and DATABASE_URL to the same
# database as a URL.
set -euo pipefail

files=()
while [ "$1" != "--" ]; do
	files+=("$1")
	shift
done
shift

dir=$(mktemp -d "${TMPDIR:-/tmp}/rusuna.XXXXXX")
trap 'pg_ctl -D "$dir/data" -m immediate stop >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT
initdb -D "$dir/data" -U postgres -A trust --no-sync >/dev/null
pg_ctl -D "$dir/data" -l "$dir/log" -o "-c listen_addresses='' -c unix_socket_directories=$dir -c shared_preload_libraries=pg_stat_statements" -w start >/dev/null
for file in "${files[@]}"; do
	psql -h "$dir" -U postgres -q -v ON_ERROR_STOP=1 -f "$file"
done
RUSUNA_POSTGRES="host=$dir user=postgres dbname=postgres" DATABASE_URL="postgres:///postgres?host=$dir&user=postgres" "$@"
