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

find "${root}/.github/workflows" -type f -name 'waddle-*.yml' -print0 |
  xargs -0 perl -0pi -e '
    my $namespace_nix_cache = "    - name: Set up Namespace Nix cache\n      uses: namespacelabs/nscloud-cache-action\@v1\n      with:\n        cache: nix\n";
    s{^    - name: Set up Namespace Nix cache\n      if: success\(\)\n      uses: namespacelabs/nscloud-cache-action\@v1\n      with:\n        cache: nix\n}{}mg;
    s{^    - name: Set up Namespace Nix cache\n      uses: namespacelabs/nscloud-cache-action\@v1\n      with:\n        cache: nix\n}{}mg;
    s{(    - name: Install Determinate Nix\n)}{$namespace_nix_cache$1}g;
  '

for workflow in \
  "${root}/.github/workflows/waddle-server-default.yml" \
  "${root}/.github/workflows/waddle-server-pullrequest.yml" \
  "${root}/.github/workflows/waddle-server-xmppcompliance.yml" \
  "${root}/.github/workflows/waddle-server-xmppcompliancepullrequest.yml"; do
  [ -f "${workflow}" ] || continue
  perl -0pi -e '
    s{^  build-cuenv:\n(?:(?!^  [A-Za-z0-9_-]+:\n).*\n)*}{}mg;
    s{^    - build-cuenv\n}{}mg;
    s{^    needs:\n(?=    steps:\n)}{}mg;
  ' "${workflow}"
done

find "${root}/.github/workflows" -type f -name '*pullrequest.yml' -print0 |
  xargs -0 perl -0pi -e '
    s{^  packages: write\n}{}mg;
  '
