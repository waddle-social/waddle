#!/usr/bin/env bash
set -euo pipefail

shopt -s nullglob
dashboard_files=(dashboards/*.json)

if ((${#dashboard_files[@]} == 0)); then
  echo "ERROR: no dashboard JSON files found under dashboards/." >&2
  exit 1
fi

seen_uids=()
seen_uid_files=()
failed=0

for dashboard_file in "${dashboard_files[@]}"; do
  if ! jq empty "${dashboard_file}"; then
    echo "ERROR: ${dashboard_file}: invalid JSON." >&2
    failed=1
    continue
  fi

  if ! jq -e '.uid | type == "string" and test("\\S")' "${dashboard_file}" >/dev/null; then
    echo "ERROR: ${dashboard_file}: .uid must be a non-empty string." >&2
    failed=1
    continue
  fi

  if ! jq -e '.title | type == "string" and test("\\S")' "${dashboard_file}" >/dev/null; then
    echo "ERROR: ${dashboard_file}: .title must be a non-empty string." >&2
    failed=1
  fi

  if ! jq -e '.panels | type == "array"' "${dashboard_file}" >/dev/null; then
    echo "ERROR: ${dashboard_file}: .panels must be an array." >&2
    failed=1
  elif ! jq -e '(.panels | length) > 0 or ((.tags | type == "array") and (.tags | index("skeleton") != null))' "${dashboard_file}" >/dev/null; then
    echo "ERROR: ${dashboard_file}: .panels must be non-empty unless .tags contains \"skeleton\"." >&2
    failed=1
  fi

  uid="$(jq -r '.uid' "${dashboard_file}")"
  duplicate_file=""
  for index in "${!seen_uids[@]}"; do
    if [[ "${seen_uids[${index}]}" == "${uid}" ]]; then
      duplicate_file="${seen_uid_files[${index}]}"
      break
    fi
  done
  if [[ -n "${duplicate_file}" ]]; then
    echo "ERROR: ${dashboard_file}: duplicate dashboard uid \"${uid}\" (already used by ${duplicate_file})." >&2
    failed=1
    continue
  fi
  seen_uids+=("${uid}")
  seen_uid_files+=("${dashboard_file}")
done

if ((failed != 0)); then
  exit 1
fi

echo "All ${#dashboard_files[@]} dashboard files lint clean."
