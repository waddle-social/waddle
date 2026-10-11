#!/usr/bin/env python3
"""Reuse a verified chart version, or publish and verify a new one."""

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


class PublicationError(Exception):
    """Chart publication cannot safely authorize the following GitOps steps."""


def contents(package: Path, name: str) -> dict[str, bytes]:
    files: dict[str, bytes] = {}
    paths: set[str] = set()
    try:
        with tarfile.open(package, 'r:gz') as archive:
            for member in archive:
                path = PurePosixPath(member.name)
                if (path.is_absolute() or '..' in path.parts or not path.parts
                        or path.parts[0] != name or str(path) in paths):
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
    except (OSError, tarfile.TarError, EOFError) as error:
        raise PublicationError(f'Cannot read chart archive: {error}') from error
    if f'{name}/Chart.yaml' not in files:
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


def existing(repository: str, name: str, version: str,
             expected: dict[str, bytes]) -> bool:
    reference = f'{repository.removeprefix("oci://").rstrip("/")}/{name}:{version}'
    with tempfile.TemporaryDirectory(prefix='published-chart-') as destination:
        result = run_helm(['pull', f'{repository.rstrip("/")}/{name}', '--version', version,
                           '--destination', destination])
        if result.returncode:
            message = output(result)
            # Only the exact requested reference's absence permits a new push.
            if not denied(message) and re.search(re.escape(reference) + r': not found(?:\s|$)', message):
                return False
            raise PublicationError(f'Cannot verify existing chart {name}:{version}: {message}')
        package = Path(destination) / f'{name}-{version}.tgz'
        if contents(package, name) != expected:
            raise PublicationError(f'Chart {name}:{version} already exists with different content; '
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


def publish(package: Path, repository: str, name: str, version: str) -> None:
    expected = contents(package, name)
    if existing(repository, name, version, expected):
        print(f'Chart {name}:{version} already exists with identical content; reusing')
        return
    for attempt in range(1, ATTEMPTS + 1):
        result = run_helm(['push', str(package), repository])
        message = output(result)
        if result.returncode == 0:
            for check in range(1, ATTEMPTS + 1):
                if existing(repository, name, version, expected):
                    print(f'Published and verified chart {name}:{version}')
                    return
                if check < ATTEMPTS:
                    time.sleep(check)
            raise PublicationError('Chart push returned success but its contents could not be verified')
        if not retryable(message):
            raise PublicationError(f'Chart publication failed: {message}')
        # A failed reply can follow a committed tag or a competing publisher.
        # Read exact contents before attempting another write.
        if existing(repository, name, version, expected):
            print(f'Chart {name}:{version} verified after an uncertain push; reusing')
            return
        if attempt == ATTEMPTS:
            raise PublicationError(f'Chart publication failed after {ATTEMPTS} attempts: {message}')
        print(f'Transient chart publication failure; retrying ({attempt}/{ATTEMPTS})', file=sys.stderr)
        time.sleep(attempt)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('package', type=Path)
    parser.add_argument('repository')
    parser.add_argument('--name', required=True)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    try:
        publish(args.package, args.repository, args.name, args.version)
    except PublicationError as error:
        print(f'Error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
