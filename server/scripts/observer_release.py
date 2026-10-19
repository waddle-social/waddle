#!/usr/bin/env python3
"""Verify a deliberately released observer artifact and its public execution policy."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


REGISTRY = 'ghcr.io/waddle-social/waddle/extensions/jev-judgments'
ARTIFACT_TYPE = 'application/vnd.waddle.extension.wasm.v1+wasm'
DIGEST = re.compile(r'sha256:[a-f0-9]{64}')
SHA256 = re.compile(r'[a-f0-9]{64}')
COMMAND_TIMEOUT = 30
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_WASM_BYTES = 32 * 1024 * 1024


class ReleaseError(Exception):
    """The observer release cannot safely authorize a build or deployment."""


def unique_fields(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ReleaseError('JSON contains a duplicate field')
        result[key] = value
    return result


def invalid_constant(_: str) -> None:
    raise ReleaseError('JSON contains a non-finite number')


def read_json(path: Path, maximum: int = MAX_MANIFEST_BYTES) -> object:
    try:
        with path.open('rb') as source:
            contents = source.read(maximum + 1)
        if len(contents) > maximum:
            raise ReleaseError('JSON exceeds the supported size')
        return json.loads(contents, object_pairs_hook=unique_fields,
                          parse_constant=invalid_constant)
    except (OSError, ValueError, UnicodeError) as error:
        raise ReleaseError('Cannot read valid JSON') from error


def load_release(path: Path) -> dict:
    release = read_json(path)
    fields = {'generation', 'registry', 'manifest_digest', 'wasm_sha256', 'policy_sha256'}
    if not isinstance(release, dict) or set(release) != fields:
        raise ReleaseError('Observer release metadata fields are invalid')
    if (type(release['generation']) is not int or release['generation'] < 1
            or release['registry'] != REGISTRY
            or not isinstance(release['manifest_digest'], str)
            or DIGEST.fullmatch(release['manifest_digest']) is None
            or release['manifest_digest'] == 'sha256:' + '0' * 64
            or any(not isinstance(release[field], str)
                   or SHA256.fullmatch(release[field]) is None
                   or release[field] == '0' * 64
                   for field in ('wasm_sha256', 'policy_sha256'))):
        raise ReleaseError('Observer release metadata values are invalid')
    return release


def policy_sha256(module: dict) -> str:
    policy = {key: value for key, value in module.items() if key != 'digest'}
    try:
        encoded = json.dumps(policy, sort_keys=True, separators=(',', ':'),
                             ensure_ascii=False, allow_nan=False).encode('utf-8')
    except (ValueError, TypeError, UnicodeError) as error:
        raise ReleaseError('Observer public policy is invalid') from error
    return hashlib.sha256(encoded).hexdigest()


def validate(release: dict, modules_path: Path) -> None:
    modules = read_json(modules_path)
    if not isinstance(modules, list) or any(not isinstance(module, dict) for module in modules):
        raise ReleaseError('Published modules must be a JSON array of objects')
    observers = [module for module in modules if module.get('name') == 'jev-judgments']
    if len(observers) != 1:
        raise ReleaseError('Published modules must contain exactly one Jev observer')
    module = observers[0]
    observation = module.get('roomObservation')
    if (not isinstance(observation, dict)
            or type(observation.get('generation')) is not int
            or observation['generation'] != release['generation']
            or module.get('registry') != release['registry']
            or module.get('digest') != release['manifest_digest']
            or policy_sha256(module) != release['policy_sha256']):
        raise ReleaseError('Observer artifact or public policy differs from the explicit release; '
                           'release a new generation before changing its identity')


def run_oras(args: list[str]) -> None:
    try:
        result = subprocess.run(['oras', *args], capture_output=True, timeout=COMMAND_TIMEOUT)
    except subprocess.TimeoutExpired as error:
        raise ReleaseError('Observer registry operation timed out') from error
    except OSError as error:
        raise ReleaseError('Cannot run the observer registry client') from error
    if result.returncode:
        # Registry stderr can contain authentication details. Never echo it.
        raise ReleaseError('Observer registry operation failed')


def manifest_layer(path: Path, release: dict) -> dict:
    try:
        with path.open('rb') as source:
            raw = source.read(MAX_MANIFEST_BYTES + 1)
    except OSError as error:
        raise ReleaseError('Observer manifest was not downloaded') from error
    if (not raw or len(raw) > MAX_MANIFEST_BYTES
            or 'sha256:' + hashlib.sha256(raw).hexdigest() != release['manifest_digest']):
        raise ReleaseError('Observer manifest does not match the immutable release digest')
    manifest = read_json(path)
    if (not isinstance(manifest, dict) or type(manifest.get('schemaVersion')) is not int
            or manifest['schemaVersion'] != 2
            or manifest.get('mediaType') != 'application/vnd.oci.image.manifest.v1+json'
            or manifest.get('artifactType') != ARTIFACT_TYPE
            or not isinstance(manifest.get('layers'), list) or len(manifest['layers']) != 1):
        raise ReleaseError('Observer manifest must contain exactly one released Wasm layer')
    layer = manifest['layers'][0]
    if (not isinstance(layer, dict) or layer.get('mediaType') != 'application/wasm'
            or layer.get('digest') != 'sha256:' + release['wasm_sha256']
            or type(layer.get('size')) is not int or not 0 < layer['size'] <= MAX_WASM_BYTES):
        raise ReleaseError('Observer Wasm layer does not match the release metadata')
    return layer


def fetch(release: dict, destination: Path) -> None:
    try:
        # Stage beside the destination so the final rename is atomic on its filesystem.
        with tempfile.TemporaryDirectory(prefix='.observer-release-', dir=destination.parent) as temporary:
            directory = Path(temporary)
            manifest = directory / 'manifest.json'
            payload = directory / 'observer.wasm'
            run_oras(['manifest', 'fetch', '--output', str(manifest),
                      release['registry'] + '@' + release['manifest_digest']])
            layer = manifest_layer(manifest, release)
            run_oras(['blob', 'fetch', '--output', str(payload),
                      release['registry'] + '@' + layer['digest']])
            if payload.stat().st_size != layer['size']:
                raise ReleaseError('Observer Wasm size does not match its manifest')
            contents = payload.read_bytes()
            if (not contents.startswith(b'\x00asm')
                    or hashlib.sha256(contents).hexdigest() != release['wasm_sha256']):
                raise ReleaseError('Observer Wasm bytes do not match the released payload')
            os.replace(payload, destination)
    except OSError as error:
        raise ReleaseError('Cannot stage the verified observer artifact') from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for command in ('digest', 'fetch', 'validate'):
        child = commands.add_parser(command)
        child.add_argument('release', type=Path)
        if command == 'fetch':
            child.add_argument('output', type=Path)
        if command == 'validate':
            child.add_argument('modules_json', type=Path)
    args = parser.parse_args()
    try:
        release = load_release(args.release)
        if args.command == 'digest':
            print(release['manifest_digest'])
        elif args.command == 'fetch':
            fetch(release, args.output)
        else:
            validate(release, args.modules_json)
    except ReleaseError as error:
        print(f'Error: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
