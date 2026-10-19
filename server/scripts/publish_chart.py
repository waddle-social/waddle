#!/usr/bin/env python3
"""Reuse a verified chart version, or publish and verify a new one.

With --check, only verify that publishing would not collide, without pushing.
"""

from __future__ import annotations

import argparse
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tempfile
import time


ATTEMPTS = 3
COMMAND_TIMEOUT = 30
CHART_NAME = 'waddle-server'
REPOSITORY = 'oci://ghcr.io/waddle-social/waddle/charts'
VERSION = re.compile(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)'
                     r'(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?'
                     r'(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?')


class PublicationError(Exception):
    """Chart publication cannot safely authorize the following GitOps steps."""


def contents(package: Path, directory: Path) -> dict[str, bytes]:
    files: dict[str, bytes] = {}
    paths: set[str] = set()
    try:
        resolved = package.resolve(strict=True)
        resolved.relative_to(directory.resolve(strict=True))
        with tarfile.open(resolved, 'r:gz') as archive:
            for member in archive:
                path = PurePosixPath(member.name)
                if (path.is_absolute() or '..' in path.parts or not path.parts
                        or path.parts[0] != CHART_NAME or str(path) in paths):
                    raise PublicationError('Chart contains an invalid or duplicate archive path')
                paths.add(str(path))
                if member.isdir():
                    continue
                if not member.isfile() or len(path.parts) < 2:
                    raise PublicationError('Chart contains a link or unsupported archive entry')
                stream = archive.extractfile(member)
                if stream is None:
                    raise PublicationError('Chart archive entry cannot be read')
                with stream:
                    files[str(path)] = stream.read()
    except (OSError, ValueError, tarfile.TarError, EOFError) as error:
        raise PublicationError(f'Cannot read chart archive: {error}') from error
    if f'{CHART_NAME}/Chart.yaml' not in files:
        raise PublicationError('Chart archive is missing Chart.yaml')
    return files


def run_helm(args: list[str]) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(['helm', *args], capture_output=True, text=True,
                              timeout=COMMAND_TIMEOUT)
    except subprocess.TimeoutExpired:
        # A timed-out write may have committed; callers re-read before retrying.
        return subprocess.CompletedProcess(['helm', *args], 1, '', 'Helm operation timed out')
    except OSError as error:
        raise PublicationError(f'Cannot run Helm: {error}') from error


def output(result: subprocess.CompletedProcess[str]) -> str:
    return (result.stdout + result.stderr).strip()


def denied(message: str) -> bool:
    return any(word in message.lower() for word in
               ('unauthorized', 'forbidden', 'denied', 'insufficient_scope'))


def existing(version: str, expected: dict[str, bytes]) -> bool:
    reference = f'{REPOSITORY.removeprefix("oci://")}/{CHART_NAME}:{version.replace("+", "_")}'
    with tempfile.TemporaryDirectory(prefix='published-chart-') as destination:
        result = run_helm(['pull', '--version', version, '--destination', destination,
                          '--', f'{REPOSITORY}/{CHART_NAME}'])
        if result.returncode:
            message = output(result)
            # Only the exact requested reference's absence permits a new push.
            if not denied(message) and re.search(re.escape(reference) + r': not found(?:\s|$)', message):
                return False
            raise PublicationError(f'Cannot verify existing chart {CHART_NAME}:{version}: {message}')
        package = Path(destination) / f'{CHART_NAME}-{version}.tgz'
        if contents(package, Path(destination)) != expected:
            raise PublicationError(f'Chart {CHART_NAME}:{version} already exists with different content; '
                                   'bump charts/waddle-server/Chart.yaml version')
        return True


def retryable(message: str) -> bool:
    if denied(message):
        return False
    lowered = message.lower()
    return (('failed to perform "tag" on destination' in lowered and 'not found' in lowered)
            or any(word in lowered for word in
                   ('already exists', '409', 'conflict', 'timed out', 'connection reset'))
            or re.search(r'(?:http|status code|response status).*\b(?:429|5\d\d)\b', lowered) is not None)


def verify_visible(version: str, expected: dict[str, bytes]) -> None:
    for check in range(1, ATTEMPTS + 1):
        if existing(version, expected):
            print(f'Published and verified chart {CHART_NAME}:{version}')
            return
        if check < ATTEMPTS:
            time.sleep(check)
    raise PublicationError('Chart push returned success but its contents could not be verified')


def package_version(package: Path) -> str:
    result = run_helm(['show', 'chart', '--', str(package)])
    if result.returncode:
        raise PublicationError(f'Cannot read chart metadata: {output(result)}')
    fields = {}
    for line in result.stdout.splitlines():
        key, _, value = line.partition(': ')
        if key in ('name', 'version'):
            if key in fields:
                raise PublicationError('Duplicate chart metadata field')
            fields[key] = value.strip().strip('\"\'')
    version = fields.get('version', '')
    if fields.get('name') != CHART_NAME or VERSION.fullmatch(version) is None:
        raise PublicationError('Invalid chart name or semantic version')
    if package.name != f'{CHART_NAME}-{version}.tgz':
        raise PublicationError('Chart package filename does not match its metadata')
    return version


def publish(package: Path, check: bool = False) -> None:
    directory = Path.cwd().resolve() / 'target' / 'charts'
    if directory.resolve() != directory:
        raise PublicationError('Chart output directory cannot be redirected by symlinks')
    expected = contents(package, directory)
    package = package.resolve(strict=True)
    version = package_version(package)
    if existing(version, expected):
        print(f'Chart {CHART_NAME}:{version} already exists with identical content; reusing')
        return
    if check:
        print(f'Chart {CHART_NAME}:{version} is unpublished; publishing will push it')
        return
    for attempt in range(1, ATTEMPTS + 1):
        result = run_helm(['push', '--', str(package), REPOSITORY])
        message = output(result)
        if result.returncode == 0:
            verify_visible(version, expected)
            return
        if not retryable(message):
            raise PublicationError(f'Chart publication failed: {message}')
        # A failed reply can follow a committed tag or a competing publisher.
        # Read exact contents before attempting another write.
        if existing(version, expected):
            print(f'Chart {CHART_NAME}:{version} verified after an uncertain push; reusing')
            return
        if attempt == ATTEMPTS:
            raise PublicationError(f'Chart publication failed after {ATTEMPTS} attempts: {message}')
        print(f'Transient chart publication failure; retrying ({attempt}/{ATTEMPTS})', file=sys.stderr)
        time.sleep(attempt)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('package', type=Path)
    parser.add_argument('--check', action='store_true', help='compare with the registry without pushing')
    args = parser.parse_args()
    try:
        publish(args.package, args.check)
    except PublicationError as error:
        print(f'Error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
