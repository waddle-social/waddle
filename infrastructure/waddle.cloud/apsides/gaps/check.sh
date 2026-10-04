#!/usr/bin/env bash
# Compiles every gap probe in a scratch copy of this project. control.tsx must
# compile. Every other probe must be rejected with the message on its
# `// Rejected with: "..."` line; a probe that compiles means Apsides gained
# that capability, and one rejected for another reason is itself broken.
#
# `aps compile` withholds compiler diagnostics, so the probes are compiled
# through the compiler library of the Apsides checkout that built `aps`.
set -euo pipefail

project="$(cd "$(dirname "$0")/.." && pwd)"
apsides="${APSIDES:?set APSIDES to an Apsides checkout with a built CLI}"
aps="$apsides/target/debug/aps"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

compile='
const [source, output] = Deno.args;
const { compileApsides } = await import("./src/compiler.ts");
try {
  await compileApsides(source, output);
  console.log("COMPILED");
} catch (error) {
  console.log(String(error instanceof Error ? error.message : error).replace(/\x1b\[[0-9;]*m/g, ""));
}
'

cp -R "$project/.aps" "$project/deno.json" "$project/gaps" "$scratch/"
status=0
for probe in "$project"/gaps/*.tsx; do
  name="$(basename "$probe" .tsx)"
  [[ "$name" == _* ]] && continue
  printf 'export { default } from "./gaps/%s.tsx";\n' "$name" > "$scratch/program.tsx"
  (cd "$scratch" && "$aps" lock --source program.tsx --modules "gaps/_base.tsx,gaps/$name.tsx" > /dev/null)
  result="$(cd "$apsides/packages/compiler" && deno eval "$compile" "$scratch/program.tsx" "$scratch/out-$name" 2> /dev/null)"
  if [[ "$name" == control ]]; then
    expected=COMPILED
  else
    expected="$(sed -n 's/^\/\/ Rejected with: "\(.*\)"\.$/\1/p' "$probe")"
  fi
  if [[ -z "$expected" ]]; then
    echo "FAIL $name: no \`// Rejected with: \"...\".\` line"; status=1
  elif [[ "$result" == *"$expected"* ]]; then
    echo "ok $name"
  elif [[ "$result" == COMPILED ]]; then
    echo "CHANGED $name: now compiles; adopt it in the program and delete the probe"; status=1
  else
    echo "FAIL $name: expected \"$expected\", got: $(head -c 300 <<< "$result")"; status=1
  fi
done
exit "$status"
