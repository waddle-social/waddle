#!/usr/bin/env python3
"""Derive acceptable *live* image revisions for the latest Recreate cutover.

The publisher supplies this list to Helm; comments and proposed values are not
rollout evidence. Helm checks these revisions against the actual ready pods.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys

MANIFEST = 'infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml'
# These inputs all trigger the server publisher. Including the whole server tree
# conservatively requires the last server build during the Recreate window.
SERVER_INPUTS = ['server', 'flake.nix', 'flake.lock', MANIFEST]


def revisions(repository: Path) -> list[str]:
    def git(*args):
        return subprocess.check_output(['git', '-C', str(repository), *args], text=True).strip()

    if git('rev-parse', '--is-shallow-repository') != 'false':
        raise ValueError('cutover guard requires complete git history; fetch --unshallow first')

    def strategy(revision):
        document = git('show', f'{revision}:{MANIFEST}')
        value = subprocess.check_output(
            ['yq', '-r', '.spec.values.updateStrategy.type'], input=document, text=True,
        ).strip()
        if value not in ('Recreate', 'RollingUpdate'):
            raise ValueError(f'{revision}: explicit Recreate or RollingUpdate strategy required')
        return value

    if strategy('HEAD') == 'Recreate':
        return []
    rolling = None
    for revision in git('log', '--first-parent', '--format=%H', 'HEAD', '--', MANIFEST).splitlines():
        if strategy(revision) == 'Recreate':
            # Any further server changes made while Recreate remained set also
            # need to have reached the fleet before the flip is safe.
            tip = git('rev-parse', f'{rolling}^')
            cutover = git('log', '-1', '--first-parent', '--format=%H', tip, '--', *SERVER_INPUTS)
            history = git('rev-list', '--first-parent', '--reverse', 'HEAD').splitlines()
            return history[history.index(cutover):]
        rolling = revision
    raise ValueError('no Recreate cutover found in history; cannot attest RollingUpdate safety')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    try:
        print(json.dumps(revisions(args.repository)))
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f'cutover guard: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
