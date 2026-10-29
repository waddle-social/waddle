#!/usr/bin/env bash
set -euo pipefail

export ROOM_CHILD_EXPERIMENT_ROOT
ROOM_CHILD_EXPERIMENT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${1:-}" != "--inside-native-shell" ]]; then
  command -v nix-shell >/dev/null
  exec nix-shell --pure "$ROOM_CHILD_EXPERIMENT_ROOT/native.nix" \
    --keep ROOM_CHILD_EXPERIMENT_ROOT --keep CARGO_TARGET_DIR \
    --run 'exec bash "$ROOM_CHILD_EXPERIMENT_ROOT/run-native.sh" --inside-native-shell'
fi

: "${ROOM_CHILD_POSTGRES_BIN:?native PostgreSQL is required}"
[[ "$(timeout --kill-after=5 10 "$ROOM_CHILD_POSTGRES_BIN/postgres" --version)" == 'postgres (PostgreSQL) 17.11' ]]
cd -- "$ROOM_CHILD_EXPERIMENT_ROOT/../.."
export CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
# Only this private Unix socket accepts connections. Trust authentication avoids
# creating any password material; directory permissions exclude other users.
umask 077
room_child_root=$(mktemp -d /tmp/waddle-room-child.XXXXXXXX)
room_child_start_attempted=0
cleanup() {
  local result=$?
  trap - EXIT INT TERM
  if (( room_child_start_attempted )) && ! "$ROOM_CHILD_POSTGRES_BIN/pg_ctl" -D "$room_child_root/data" -w -t 30 -m fast stop >/dev/null 2>&1; then
    echo "PostgreSQL shutdown unconfirmed; retaining $room_child_root" >&2
    exit 1
  fi
  rm -rf -- "$room_child_root"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$room_child_root/socket"
unset PGHOSTADDR PGPASSWORD PGOPTIONS PGSERVICE PGSERVICEFILE
export PGPASSFILE="$room_child_root/no-password-file"
export PGHOST="$room_child_root/socket" PGPORT=55432 PGUSER=waddle_test PGDATABASE=room_child
timeout --kill-after=5 60 "$ROOM_CHILD_POSTGRES_BIN/initdb" -D "$room_child_root/data" -U "$PGUSER" --auth-local=trust --auth-host=reject --no-locale -E UTF8 >/dev/null
room_child_start_attempted=1
"$ROOM_CHILD_POSTGRES_BIN/pg_ctl" -D "$room_child_root/data" -l /dev/null -w -t 30 \
  -o "-k $PGHOST -p $PGPORT -c listen_addresses='' -c logging_collector=off -c log_statement=none -c log_min_error_statement=panic" start >/dev/null
timeout --kill-after=5 30 "$ROOM_CHILD_POSTGRES_BIN/createdb" "$PGDATABASE"
export WADDLE_TEST_POSTGRES_URL="postgresql://$PGUSER@localhost/$PGDATABASE?host=$PGHOST&port=$PGPORT"
# Compiling and test execution have independent bounded deadlines.
timeout --kill-after=10 1800 cargo test -p waddle-server --lib --features test-support --locked \
  --no-run --message-format=json-render-diagnostics > "$room_child_root/build.json"
room_child_binary=$(python3 - "$room_child_root/build.json" <<'PY'
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
timeout --kill-after=5 30 "$room_child_binary" room_child_contract:: --ignored --list > "$room_child_root/tests.txt"
python3 - "$room_child_root/tests.txt" <<'PY'
import sys
prefix = "ingress::recovery_executor::tests::muc::room_child_contract::"
expected = {
    prefix + "postgres_sealed_child_delayed_release_preserves_payload_and_target",
    prefix + "postgres_sealed_child_leave_rejoin_retires_original_without_delivery",
    prefix + "postgres_sealed_child_rejects_changed_payload_or_target_before_sink",
    prefix + "postgres_sealed_children_destroy_recreate_retire_without_transport",
    prefix + "postgres_sealed_manifest_preserves_children_after_durable_revision_advance",
}
with open(sys.argv[1]) as source:
    actual = {line.removesuffix(": test\n") for line in source if line.endswith(": test\n")}
if actual != expected:
    raise SystemExit(f"test inventory mismatch: missing={expected - actual}, extra={actual - expected}")
print("Native PostgreSQL 17.11: exact five-test inventory verified")
PY
timeout --kill-after=10 180 "$room_child_binary" room_child_contract:: --ignored --test-threads=1 \
  | tee "$room_child_root/results.txt"
python3 - "$room_child_root/results.txt" <<'PY'
import re
import sys
with open(sys.argv[1]) as source:
    results = source.read()
if not re.search(r"^test result: ok\. 5 passed; 0 failed; 0 ignored;", results, re.MULTILINE):
    raise SystemExit("expected five executed passing PostgreSQL tests, none ignored")
PY
