#!/usr/bin/env python3
"""Exercise the publisher's actual chart step without writing to a registry."""

from __future__ import annotations

import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


SERVER = Path(__file__).resolve().parents[1]
TAG_ERROR = 'Error: failed to perform "Tag" on destination: sha256:missing: not found'

FAKE_HELM = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
log = pathlib.Path(os.environ['CHART_FIXTURE_LOG'])
previous = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
with log.open('a') as out:
    out.write(json.dumps(args) + '\n')
mode = os.environ['CHART_FIXTURE_MODE']
pushes = sum(call[0] == 'push' for call in previous)
if args[0] == 'package':
    print('Successfully packaged chart and saved it to: ' + os.environ['CHART_FIXTURE_LOCAL'])
elif args[:2] == ['show', 'chart']:
    print('name: waddle-server\nversion: 0.4.7')
elif args[0] == 'pull':
    if mode == 'pull-denied':
        print('Error: 403 Forbidden: insufficient_scope', file=sys.stderr)
        sys.exit(1)
    if mode == 'pull-unknown':
        print('Error: registry returned an unexpected response', file=sys.stderr)
        sys.exit(1)
    present = (mode in ('same', 'different', 'malformed')
               or mode.startswith('race-') and pushes > 0
               or mode in ('missing', 'success-different') and pushes > 0
               or mode == 'transient' and pushes >= 2)
    if not present:
        print('Error: ghcr.io/waddle-social/waddle/charts/waddle-server:0.4.7: not found', file=sys.stderr)
        sys.exit(1)
    destination = pathlib.Path(args[args.index('--destination') + 1])
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(os.environ['CHART_FIXTURE_REMOTE'], destination / 'waddle-server-0.4.7.tgz')
elif args[0] == 'push':
    if mode in ('same', 'different', 'pull-denied', 'malformed', 'exhausted') or mode == 'transient' and pushes == 0:
        print('Error: failed to perform "Tag" on destination: sha256:missing: not found', file=sys.stderr)
        sys.exit(1)
    if mode == 'push-denied':
        print('Error: 403 Forbidden: denied', file=sys.stderr)
        sys.exit(1)
    if mode.startswith('race-'):
        print('Error: 409 Conflict: already exists', file=sys.stderr)
        sys.exit(1)
    print('Pushed chart')
else:
    raise SystemExit('Unexpected Helm invocation: ' + repr(args))
'''


def archive(path: Path, template: bytes, timestamp: int) -> None:
    files = {
        'waddle-server/Chart.yaml': b'apiVersion: v2\nname: waddle-server\nversion: 0.4.7\n',
        'waddle-server/values.yaml': b'replicas: 2\n',
        'waddle-server/templates/deployment.yaml': template,
    }
    with tarfile.open(path, 'w:gz') as out:
        for name, contents in files.items():
            member = tarfile.TarInfo(name)
            member.size = len(contents)
            member.mtime = timestamp
            out.addfile(member, io.BytesIO(contents))


class PublisherChartTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix='publisher-chart-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.local = self.root / 'local.tgz'
        self.remote = self.root / 'remote.tgz'
        archive(self.local, b'kind: Deployment\n', 1)
        archive(self.remote, b'kind: Deployment\n', 2)
        self.log = self.root / 'commands.jsonl'
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        helm = self.bin / 'helm'
        helm.write_text(FAKE_HELM)
        helm.chmod(0o755)
        (self.root / 'scripts').symlink_to(SERVER / 'scripts', target_is_directory=True)

    def run_chart_step(self, mode: str) -> subprocess.CompletedProcess[str]:
        lines = (SERVER / 'env.cue').read_text().splitlines()
        start = next(i for i, line in enumerate(lines) if line.lstrip().startswith('chart_package='))
        end = next(i for i in range(start, len(lines)) if ' | oras login ghcr.io' in lines[i])
        script = ('set -euo pipefail\n' + '\n'.join(line.lstrip('\t') for line in lines[start:end])
                  + '\necho chart-publication-complete\n')
        env = os.environ.copy()
        env.update({
            'PATH': str(self.bin) + os.pathsep + env['PATH'],
            'CHART_FIXTURE_MODE': mode,
            'CHART_FIXTURE_LOG': str(self.log),
            'CHART_FIXTURE_LOCAL': str(self.local),
            'CHART_FIXTURE_REMOTE': str(self.remote),
        })
        return subprocess.run(['bash', '-c', script], cwd=self.root, env=env, text=True,
                              capture_output=True, timeout=20)

    def pushes(self) -> list[list[str]]:
        return [call for call in self.commands() if call[0] == 'push']

    def commands(self) -> list[list[str]]:
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def assert_success(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('chart-publication-complete', result.stdout)

    def assert_stopped(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn('chart-publication-complete', result.stdout)

    def test_existing_identical_chart_is_reused_without_push(self) -> None:
        self.assertNotEqual(self.local.read_bytes(), self.remote.read_bytes())
        self.assert_success(self.run_chart_step('same'))
        self.assertEqual(self.pushes(), [])

    def test_changed_existing_version_requires_bump_without_push(self) -> None:
        archive(self.remote, b'kind: ChangedDeployment\n', 2)
        result = self.run_chart_step('different')
        self.assert_stopped(result)
        self.assertIn('bump', result.stdout + result.stderr)
        self.assertEqual(self.pushes(), [])

    def test_missing_version_is_published(self) -> None:
        self.assert_success(self.run_chart_step('missing'))
        self.assertEqual(len(self.pushes()), 1)
        self.assertEqual(self.pushes()[0][1:], [str(self.local), 'oci://ghcr.io/waddle-social/waddle/charts'])

    def test_unknown_pull_permission_error_cannot_authorize_push(self) -> None:
        result = self.run_chart_step('pull-denied')
        self.assert_stopped(result)
        self.assertEqual(self.pushes(), [])

    def test_push_permission_failure_is_not_retried(self) -> None:
        result = self.run_chart_step('push-denied')
        self.assert_stopped(result)
        self.assertEqual(len(self.pushes()), 1)

    def test_transient_tag_not_found_is_retried_with_same_package(self) -> None:
        self.assert_success(self.run_chart_step('transient'))
        self.assertEqual(len(self.pushes()), 2)
        self.assertEqual(self.pushes()[0], self.pushes()[1])

    def test_transient_failure_has_three_attempt_ceiling(self) -> None:
        result = self.run_chart_step('exhausted')
        self.assert_stopped(result)
        self.assertEqual(len(self.pushes()), 3)

    def test_concurrent_identical_publisher_is_accepted(self) -> None:
        self.assert_success(self.run_chart_step('race-same'))
        self.assertEqual(len(self.pushes()), 1)

    def test_concurrent_different_publisher_requires_bump(self) -> None:
        archive(self.remote, b'kind: ChangedDeployment\n', 2)
        result = self.run_chart_step('race-different')
        self.assert_stopped(result)
        self.assertIn('bump', result.stdout + result.stderr)
        self.assertEqual(len(self.pushes()), 1)

    def test_malformed_remote_chart_cannot_authorize_push(self) -> None:
        self.remote.write_bytes(b'not a chart archive')
        result = self.run_chart_step('malformed')
        self.assert_stopped(result)
        self.assertEqual(self.pushes(), [])

    def test_unknown_pull_failure_cannot_authorize_push(self) -> None:
        self.assert_stopped(self.run_chart_step('pull-unknown'))
        self.assertEqual(self.pushes(), [])

    def test_successful_push_requires_matching_remote_contents(self) -> None:
        archive(self.remote, b'kind: ChangedDeployment\n', 2)
        result = self.run_chart_step('success-different')
        self.assert_stopped(result)
        self.assertIn('bump', result.stderr)
        self.assertEqual(len(self.pushes()), 1)

    def test_successful_push_with_invisible_tag_is_not_accepted(self) -> None:
        self.assert_stopped(self.run_chart_step('invisible-success'))
        self.assertEqual(len(self.pushes()), 1)

    def test_unsafe_remote_archive_entries_are_rejected(self) -> None:
        for kind in ('duplicate', 'traversal', 'absolute', 'symlink', 'hardlink', 'special', 'wrong-root', 'missing-chart'):
            with self.subTest(kind=kind):
                self.log.unlink(missing_ok=True)
                with tarfile.open(self.remote, 'w:gz') as out:
                    name = 'waddle-server/Chart.yaml'
                    if kind == 'traversal':
                        name = 'waddle-server/../secret'
                    elif kind == 'absolute':
                        name = '/waddle-server/Chart.yaml'
                    elif kind == 'wrong-root':
                        name = 'foreign/Chart.yaml'
                    elif kind == 'missing-chart':
                        name = 'waddle-server/values.yaml'
                    entry = tarfile.TarInfo(name)
                    if kind in ('symlink', 'hardlink', 'special'):
                        entry.type = {'symlink': tarfile.SYMTYPE, 'hardlink': tarfile.LNKTYPE,
                                      'special': tarfile.FIFOTYPE}[kind]
                        entry.linkname = '/outside'
                    out.addfile(entry, io.BytesIO(b''))
                    if kind == 'duplicate':
                        out.addfile(entry, io.BytesIO(b''))
                self.assert_stopped(self.run_chart_step('malformed'))
                self.assertEqual(self.pushes(), [])


if __name__ == '__main__':
    unittest.main()
