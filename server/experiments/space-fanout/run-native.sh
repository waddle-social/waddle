#!/usr/bin/env bash
set -euo pipefail

export SPACE_FANOUT_EXPERIMENT_ROOT
SPACE_FANOUT_EXPERIMENT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${1:-}" != "--inside-native-shell" ]]; then
  command -v nix-shell >/dev/null
  exec nix-shell --pure "$SPACE_FANOUT_EXPERIMENT_ROOT/native.nix" \
    --keep SPACE_FANOUT_EXPERIMENT_ROOT --keep CARGO_TARGET_DIR \
    --run 'exec bash "$SPACE_FANOUT_EXPERIMENT_ROOT/run-native.sh" --inside-native-shell'
fi

: "${SPACE_FANOUT_POSTGRES_BIN:?native PostgreSQL is required}"
[[ "$(timeout --kill-after=5 10 "$SPACE_FANOUT_POSTGRES_BIN/postgres" --version)" == 'postgres (PostgreSQL) 17.11' ]]
cd -- "$SPACE_FANOUT_EXPERIMENT_ROOT/../.."
export CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
# Only this private Unix socket accepts connections. Trust authentication avoids
# creating any password material; directory permissions exclude other users.
umask 077
space_fanout_root=$(mktemp -d /tmp/waddle-space-fanout.XXXXXXXX)
space_fanout_start_attempted=0
cleanup() {
  local result=$?
  trap - EXIT INT TERM
  if (( space_fanout_start_attempted )) && ! "$SPACE_FANOUT_POSTGRES_BIN/pg_ctl" -D "$space_fanout_root/data" -w -t 30 -m fast stop >/dev/null 2>&1; then
    echo "PostgreSQL shutdown unconfirmed; retaining $space_fanout_root" >&2
    exit 1
  fi
  rm -rf -- "$space_fanout_root"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$space_fanout_root/socket"
unset PGHOSTADDR PGPASSWORD PGOPTIONS PGSERVICE PGSERVICEFILE
export PGPASSFILE="$space_fanout_root/no-password-file"
export PGHOST="$space_fanout_root/socket" PGPORT=55432 PGUSER=waddle_test PGDATABASE=space_fanout
timeout --kill-after=5 60 "$SPACE_FANOUT_POSTGRES_BIN/initdb" -D "$space_fanout_root/data" -U "$PGUSER" --auth-local=trust --auth-host=reject --no-locale -E UTF8 >/dev/null
space_fanout_start_attempted=1
"$SPACE_FANOUT_POSTGRES_BIN/pg_ctl" -D "$space_fanout_root/data" -l /dev/null -w -t 30 \
  -o "-k $PGHOST -p $PGPORT -c listen_addresses='' -c logging_collector=off -c log_statement=none -c log_min_error_statement=panic" start >/dev/null
timeout --kill-after=5 30 "$SPACE_FANOUT_POSTGRES_BIN/createdb" "$PGDATABASE"
export WADDLE_TEST_POSTGRES_URL="postgresql://$PGUSER@localhost/$PGDATABASE?host=$PGHOST&port=$PGPORT"
# Compiling and test execution have independent bounded deadlines.
timeout --kill-after=10 1800 cargo test -p waddle-server --lib --features test-support --locked \
  --no-run --message-format=json-render-diagnostics > "$space_fanout_root/build.json"
space_fanout_binary=$(python3 - "$space_fanout_root/build.json" <<'PY'
import json
import sys
binaries = []
with open(sys.argv[1]) as source:
    for line in source:
        event = json.loads(line)
        if (event.get("reason") == "compiler-artifact"
                and event["target"]["name"] == "waddle_server"
                and event["profile"]["test"] and event.get("executable")):
            binaries.append(event["executable"])
if len(binaries) != 1:
    raise SystemExit(f"expected exactly one server library test binary, got {len(binaries)}")
print(binaries[0])
PY
)
timeout --kill-after=5 30 "$space_fanout_binary" space_fanout_postgres:: --ignored --list > "$space_fanout_root/tests.txt"
python3 - "$space_fanout_root/tests.txt" <<'PY'
import sys
prefix = "server::routes::websocket::tests::space_fanout_postgres::"
expected = {prefix + "postgres_space_fanout_outcast_after_capture_negative_control"}
with open(sys.argv[1]) as source:
    actual = {line.removesuffix(": test\n") for line in source if line.endswith(": test\n")}
if actual != expected:
    raise SystemExit(f"test inventory mismatch: missing={expected - actual}, extra={actual - expected}")
print("Native PostgreSQL 17.11: exact one-test inventory verified")
PY
timeout --kill-after=10 180 "$space_fanout_binary" space_fanout_postgres:: --ignored --test-threads=1 \
  | tee "$space_fanout_root/results.txt"
python3 - "$space_fanout_root/results.txt" <<'PY'
import re
import sys
with open(sys.argv[1]) as source:
    results = source.read()
if not re.search(r"^test result: ok\. 1 passed; 0 failed; 0 ignored;", results, re.MULTILINE):
    raise SystemExit("expected one executed passing PostgreSQL tests, none ignored")
PY
