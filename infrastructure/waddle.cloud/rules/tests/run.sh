#!/bin/sh
# Run from any directory with promtool on PATH. No ruler or credentials needed.
set -eu
rules_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT HUP INT TERM

# namespace is Mimir upload metadata, not part of Prometheus rule syntax.
sed '/^namespace:/d' "$rules_dir/mimir/waddle-reliability.yaml" > "$test_dir/waddle-reliability.yaml"
cp "$rules_dir/tests/ingress.test.yaml" "$test_dir/ingress.test.yaml"
promtool check rules "$test_dir/waddle-reliability.yaml"
promtool test rules "$test_dir/ingress.test.yaml"
