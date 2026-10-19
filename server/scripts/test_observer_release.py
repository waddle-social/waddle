#!/usr/bin/env python3
"""Exercise immutable observer releases without registry writes or secrets."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import observer_release as observer


SERVER = Path(__file__).resolve().parents[1]
OLD_MANIFEST = b'{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"application/vnd.waddle.extension.wasm.v1+wasm","config":{"mediaType":"application/vnd.oci.empty.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2,"data":"e30="},"layers":[{"mediaType":"application/wasm","digest":"sha256:1978f10bffa42cc58b61bd6a8a4d4418e4a0110aa10d3958c4690c9bb10970df","size":446651,"annotations":{"org.opencontainers.image.title":"target/wasm32-wasip2/release/jev_judgments.wasm"}}],"annotations":{"org.opencontainers.image.created":"2026-10-18T21:17:44Z"}}'


class ObserverReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix='observer-release-test-')
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.output = self.directory / 'released.wasm'
        self.output.write_bytes(b'previous verified artifact')
        self.payload = b'\x00asm\x0d\x00\x01\x00fixture component'
        self.release = observer.load_release(SERVER / 'observer-release.json')
        self.release['wasm_sha256'] = hashlib.sha256(self.payload).hexdigest()
        self.manifest = json.loads(OLD_MANIFEST)
        self.manifest['layers'][0]['digest'] = 'sha256:' + self.release['wasm_sha256']
        self.manifest['layers'][0]['size'] = len(self.payload)
        self.pin_manifest()
        self.calls = []

    def pin_manifest(self, raw: bytes | None = None) -> None:
        self.raw_manifest = raw if raw is not None else json.dumps(self.manifest).encode()
        self.release['manifest_digest'] = 'sha256:' + hashlib.sha256(self.raw_manifest).hexdigest()

    def registry(self, args: list[str], **kwargs) -> subprocess.CompletedProcess:
        self.assertEqual(kwargs['timeout'], 30)
        self.assertTrue(kwargs['capture_output'])
        self.calls.append(args)
        self.assertEqual(args[0], 'oras')
        destination = Path(args[args.index('--output') + 1])
        if args[1:3] == ['manifest', 'fetch']:
            self.assertEqual(args[-1], observer.REGISTRY + '@' + self.release['manifest_digest'])
            destination.write_bytes(self.raw_manifest)
        elif args[1:3] == ['blob', 'fetch']:
            self.assertEqual(args[-1], observer.REGISTRY + '@sha256:' + self.release['wasm_sha256'])
            destination.write_bytes(self.payload)
        else:
            self.fail('Unexpected registry command')
        return subprocess.CompletedProcess(args, 0, b'', b'')

    def fetch(self) -> None:
        with patch.object(observer.subprocess, 'run', side_effect=self.registry):
            observer.fetch(self.release, self.output)

    def assert_fetch_rejected(self) -> None:
        with self.assertRaises(observer.ReleaseError):
            self.fetch()
        self.assertEqual(self.output.read_bytes(), b'previous verified artifact')
        self.assertEqual(list(self.directory.iterdir()), [self.output])

    def module(self) -> dict:
        return {
            'name': 'jev-judgments', 'registry': self.release['registry'],
            'digest': self.release['manifest_digest'], 'namespace': 'urn:waddle:safety-scores:1',
            'config': {'model': 'fixture', 'endpoint': 'https://example.org'},
            'configSecretFiles': {'api_key': '/mounted/secret'},
            'capabilityGrants': ['message.observe'],
            'roomObservation': {'generation': self.release['generation'], 'scope': {'kind': 'all-hosted-rooms'}},
        }

    def validate(self, modules: object) -> None:
        path = self.directory / 'modules.json'
        path.write_text(json.dumps(modules))
        observer.validate(self.release, path)

    def test_checked_in_release_matches_actual_generation_two_manifest(self) -> None:
        release = observer.load_release(SERVER / 'observer-release.json')
        path = self.directory / 'actual-manifest.json'
        path.write_bytes(OLD_MANIFEST)
        layer = observer.manifest_layer(path, release)
        self.assertEqual(release['generation'], 2)
        self.assertEqual(layer['digest'], 'sha256:1978f10bffa42cc58b61bd6a8a4d4418e4a0110aa10d3958c4690c9bb10970df')
        self.assertEqual(layer['size'], 446651)

    def test_fetch_verifies_both_digests_and_atomically_replaces_output(self) -> None:
        # A hostile archive-style annotation cannot affect the flat output path.
        self.manifest['layers'][0]['annotations']['org.opencontainers.image.title'] = '../../escape'
        self.pin_manifest()
        self.fetch()
        self.assertEqual(self.output.read_bytes(), self.payload)
        self.assertEqual(len(self.calls), 2)
        self.assertEqual(list(self.directory.iterdir()), [self.output])

    def test_wrong_manifest_digest_rejected_before_blob_download(self) -> None:
        self.raw_manifest += b' '
        self.assert_fetch_rejected()
        self.assertEqual(len(self.calls), 1)

    def test_empty_and_malformed_manifest_rejected(self) -> None:
        for raw in (b'', b'{broken', b'null', b'[]', b'{"schemaVersion":NaN}'):
            with self.subTest(raw=raw):
                self.pin_manifest(raw)
                self.assert_fetch_rejected()

    def test_missing_manifest_and_payload_rejected(self) -> None:
        for missing in ('manifest', 'blob'):
            with self.subTest(missing=missing):
                def registry(args: list[str], **kwargs):
                    if args[1] == missing:
                        return subprocess.CompletedProcess(args, 0, b'', b'')
                    return self.registry(args, **kwargs)
                with patch.object(observer.subprocess, 'run', side_effect=registry):
                    with self.assertRaises(observer.ReleaseError):
                        observer.fetch(self.release, self.output)
                self.assertEqual(self.output.read_bytes(), b'previous verified artifact')

    def test_unexpected_manifest_layers_rejected(self) -> None:
        original = copy.deepcopy(self.manifest)
        cases = ([], original['layers'] * 2, [{}], [dict(original['layers'][0], mediaType='application/gzip')],
                 [dict(original['layers'][0], digest='sha256:' + '1' * 64)],
                 [dict(original['layers'][0], size=0)], [dict(original['layers'][0], size=True)],
                 [dict(original['layers'][0], size=observer.MAX_WASM_BYTES + 1)])
        for layers in cases:
            with self.subTest(layers=layers):
                self.manifest = dict(original, layers=layers)
                self.pin_manifest()
                self.assert_fetch_rejected()

    def test_wrong_artifact_type_rejected(self) -> None:
        self.manifest['artifactType'] = 'application/gzip'
        self.pin_manifest()
        self.assert_fetch_rejected()

    def test_empty_truncated_and_wrong_payload_rejected(self) -> None:
        for payload in (b'', self.payload[:-1], self.payload[:-1] + b'x'):
            with self.subTest(payload=payload):
                self.payload = payload
                self.assert_fetch_rejected()

    def test_matching_hash_without_wasm_magic_rejected(self) -> None:
        self.payload = b'not a wasm artifact'
        self.release['wasm_sha256'] = hashlib.sha256(self.payload).hexdigest()
        self.manifest['layers'][0].update(digest='sha256:' + self.release['wasm_sha256'], size=len(self.payload))
        self.pin_manifest()
        self.assert_fetch_rejected()

    def test_registry_timeout_failure_and_missing_client_do_not_publish_output(self) -> None:
        for stage in ('manifest', 'blob'):
            for failure in ('timeout', 'failure', 'missing-client'):
                with self.subTest(stage=stage, failure=failure):
                    def registry(args: list[str], **kwargs):
                        if args[1] != stage:
                            return self.registry(args, **kwargs)
                        if failure == 'timeout':
                            raise subprocess.TimeoutExpired(args, 30, stderr=b'secret=do-not-log')
                        if failure == 'missing-client':
                            raise OSError('secret=do-not-log')
                        return subprocess.CompletedProcess(args, 1, b'', b'secret=do-not-log')
                    with patch.object(observer.subprocess, 'run', side_effect=registry):
                        with self.assertRaises(observer.ReleaseError) as raised:
                            observer.fetch(self.release, self.output)
                    self.assertNotIn('do-not-log', str(raised.exception))
                    self.assertEqual(self.output.read_bytes(), b'previous verified artifact')

    def test_same_generation_rejects_any_public_policy_change(self) -> None:
        module = self.module()
        self.release['policy_sha256'] = observer.policy_sha256(module)
        self.validate([module])
        for key, value in (('config', {'model': 'changed'}), ('capabilityGrants', ['outbound.http.request']),
                           ('configSecretFiles', {'api_key': '/different/path'}), ('newPublicField', True),
                           ('roomObservation', dict(module['roomObservation'], generation=3))):
            with self.subTest(key=key):
                with self.assertRaises(observer.ReleaseError):
                    self.validate([dict(module, **{key: value})])

    def test_new_build_digest_does_not_authorize_same_release(self) -> None:
        module = self.module()
        self.release['policy_sha256'] = observer.policy_sha256(module)
        with self.assertRaises(observer.ReleaseError):
            self.validate([dict(module, digest='sha256:' + '1' * 64)])

    def test_explicit_new_release_accepts_new_generation_artifact_and_policy(self) -> None:
        module = self.module()
        module['roomObservation']['generation'] = 3
        module['config']['model'] = 'deliberately-released-model'
        module['digest'] = 'sha256:' + '1' * 64
        self.release.update(generation=3, manifest_digest=module['digest'],
                            policy_sha256=observer.policy_sha256(module))
        self.validate([module])

    def test_missing_duplicate_and_malformed_modules_rejected(self) -> None:
        module = self.module()
        self.release['policy_sha256'] = observer.policy_sha256(module)
        for modules in ([], [module, module], {}, [None], [dict(module, roomObservation=None)]):
            with self.subTest(modules=modules):
                with self.assertRaises(observer.ReleaseError):
                    self.validate(modules)

    def test_metadata_and_duplicate_json_fields_rejected(self) -> None:
        path = self.directory / 'release.json'
        for key, value in (('generation', True), ('generation', 0), ('registry', 'other/repository'),
                           ('manifest_digest', 'latest'), ('wasm_sha256', '0' * 64),
                           ('policy_sha256', None), ('unexpected', 1)):
            with self.subTest(key=key, value=value):
                path.write_text(json.dumps(dict(self.release, **{key: value})))
                with self.assertRaises(observer.ReleaseError):
                    observer.load_release(path)
        path.write_text('{"generation":2,"generation":3}')
        with self.assertRaises(observer.ReleaseError):
            observer.load_release(path)

    def test_digest_cli_prints_only_the_immutable_manifest_digest(self) -> None:
        result = subprocess.run([sys.executable, str(SERVER / 'scripts/observer_release.py'), 'digest',
                                 str(SERVER / 'observer-release.json')], capture_output=True,
                                text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, 'sha256:c37619841f9da5861887651779337fe8467f41910566267063b8b596dee379dc\n')
        self.assertEqual(result.stderr, '')


if __name__ == '__main__':
    unittest.main()
