#!/usr/bin/env bash
set -euo pipefail

# The Python runner owns the lock, builds, gate, and atomic evidence write.
exec python3 "$(dirname "${BASH_SOURCE[0]}")/run_nextest_shards.py" "$@"
