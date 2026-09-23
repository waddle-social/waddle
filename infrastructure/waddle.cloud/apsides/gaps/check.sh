#!/usr/bin/env bash
# Compiles every gap probe in a scratch copy of this project. control.tsx must
# compile and every other probe must fail; a probe that starts compiling means
# Apsides gained that capability and the program should adopt it.
#
# `aps compile` withholds compiler diagnostics, so a failure here shows only
# that the probe was rejected. Each probe's header names the rejection its
# author verified.
set -euo pipefail

project="$(cd "$(dirname "$0")/.." && pwd)"
aps="${APS:?set APS to an aps binary built from an Apsides checkout}"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

cp -R "$project/.aps" "$project/deno.json" "$project/gaps" "$scratch/"
status=0
for probe in "$project"/gaps/*.tsx; do
  name="$(basename "$probe" .tsx)"
  [[ "$name" == _* ]] && continue
  printf 'export { default } from "./gaps/%s.tsx";\n' "$name" > "$scratch/program.tsx"
  (cd "$scratch" && "$aps" lock --source program.tsx --modules "gaps/_base.tsx,gaps/$name.tsx" > /dev/null)
  if "$aps" compile --source "$scratch/program.tsx" --output "$scratch/out-$name" > /dev/null 2>&1; then
    compiled=yes
  else
    compiled=no
  fi
  if [[ "$name" == control && "$compiled" == no ]]; then
    echo "FAIL control: the probe scaffolding does not compile"; status=1
  elif [[ "$name" != control && "$compiled" == yes ]]; then
    echo "CHANGED $name: now compiles; adopt it in the program and delete the probe"; status=1
  else
    echo "ok $name"
  fi
done
exit "$status"
