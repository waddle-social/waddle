#!/bin/sh
# Run from any directory with promtool on PATH. No ruler or credentials needed.
set -eu
rules_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT HUP INT TERM

# Every Mimir rule file is checked and every *.test.yaml is evaluated, so a
# new rule file or test file cannot be silently skipped. Test files name
# their rule files by basename (rule_files:), resolved inside $test_dir.
# namespace is Mimir upload metadata, not part of Prometheus rule syntax.
rule_files=0
for rule_file in "$rules_dir"/mimir/*.yaml; do
  [ -e "$rule_file" ] || continue
  sed '/^namespace:/d' "$rule_file" > "$test_dir/$(basename -- "$rule_file")"
  promtool check rules "$test_dir/$(basename -- "$rule_file")"
  rule_files=$((rule_files + 1))
done
[ "$rule_files" -gt 0 ] || { echo "no Mimir rule files under $rules_dir/mimir" >&2; exit 1; }

test_files=0
for test_file in "$rules_dir"/tests/*.test.yaml; do
  [ -e "$test_file" ] || continue
  cp "$test_file" "$test_dir/$(basename -- "$test_file")"
  promtool test rules "$test_dir/$(basename -- "$test_file")"
  test_files=$((test_files + 1))
done
[ "$test_files" -gt 0 ] || { echo "no promtool test files under $rules_dir/tests" >&2; exit 1; }

# Optional bounded end-to-end contract: uses an existing Prometheus binary
# with its real OTLP receiver/translator; never downloads tools or needs a cloud.
if [ -n "${PROMETHEUS_BIN:-}" ]; then
  python3 "$rules_dir/tests/push_otlp_contract.py" --prometheus "$PROMETHEUS_BIN"
fi
