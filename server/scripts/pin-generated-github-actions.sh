#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"

find "${root}/.github/workflows" -type f -name '*.yml' -print0 |
  xargs -0 perl -0pi -e '
    s{actions/checkout\@v4}{actions/checkout\@34e114876b0b11c390a56381ad16ebd13914f8d5}g;
    s{actions/checkout\@v6}{actions/checkout\@de0fac2e4500dabe0009e67214ff5f5447ce83dd}g;
    s{actions/upload-artifact\@v4}{actions/upload-artifact\@ea165f8d65b6e75b540449e92b4886f43607fa02}g;
    s{^        GITHUB_ACTOR: \$\{\{ github\.actor \}\}\n}{}mg;
    s{^        GITHUB_REF_TYPE: \$\{\{ github\.ref_type \}\}\n}{}mg;
    s{^        GITHUB_REF_NAME: \$\{\{ github\.ref_name \}\}\n}{}mg;
  '
