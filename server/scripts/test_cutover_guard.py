import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

CHART = Path(__file__).resolve().parents[1] / 'charts/waddle-server'
SHA = '1' * 40


class CutoverGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.chart = Path(self.temp.name)
        (self.chart / 'templates').mkdir()
        (self.chart / 'Chart.yaml').write_text('apiVersion: v2\nname: guard-test\nversion: 0.1.0\n')
        helper = CHART / 'templates/_cutover-guard.tpl'
        if helper.exists():
            shutil.copy(helper, self.chart / 'templates/_cutover-guard.tpl')
        (self.chart / 'templates/check.yaml').write_text('{{ include "waddle-server.guardCutover" .Values.fixture }}')
        self.fixture = {
            'allowedRevisions': [SHA],
            'deployment': {
                'metadata': {'generation': 4},
                'spec': {'replicas': 2, 'selector': {'matchLabels': {'app': 'waddle'}}},
                'status': {'observedGeneration': 4, 'replicas': 2, 'updatedReplicas': 2, 'readyReplicas': 2, 'availableReplicas': 2},
            },
            'pods': {'items': [self.pod('one'), self.pod('two')]},
        }

    def pod(self, name):
        return {
            'metadata': {'name': name, 'labels': {'app': 'waddle'}},
            'spec': {'containers': [{'name': 'waddle-server', 'image': 'waddle@sha256:' + 'a' * 64,
                                     'env': [{'name': 'WADDLE_GIT_SHA', 'value': SHA}]}]},
            'status': {'phase': 'Running', 'conditions': [{'type': 'Ready', 'status': 'True'}]},
        }

    def render(self):
        values = self.chart / 'values.json'
        values.write_text(json.dumps({'fixture': self.fixture}))
        return subprocess.run(['helm', 'template', 'test', str(self.chart), '-f', str(values)], text=True, capture_output=True)

    def test_completed_cutover_can_flip(self):
        result = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_stale_observation_missing_pods_and_mixed_rollout_fail_closed(self):
        for key in ('observedGeneration', 'replicas', 'updatedReplicas', 'readyReplicas', 'availableReplicas'):
            with self.subTest(key=key):
                previous = self.fixture['deployment']['status'][key]
                self.fixture['deployment']['status'][key] = 0
                result = self.render()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('fully observed, ready Deployment', result.stderr)
                self.fixture['deployment']['status'][key] = previous

    def test_empty_or_missing_live_objects_fail_closed(self):
        for key, empty in [('allowedRevisions', []), ('deployment', {}), ('pods', {'items': []})]:
            with self.subTest(key=key):
                previous = self.fixture[key]
                self.fixture[key] = empty
                self.assertNotEqual(self.render().returncode, 0)
                self.fixture[key] = previous

    def test_terminating_or_unready_server_pod_blocks_flip(self):
        pod = self.fixture['pods']['items'][0]
        pod['metadata']['deletionTimestamp'] = '2026-09-24T00:00:00Z'
        self.assertIn('nonterminating', self.render().stderr)
        del pod['metadata']['deletionTimestamp']
        pod['status']['conditions'] = []
        self.assertIn('every server pod ready', self.render().stderr)

    def test_missing_revision_and_mutable_image_block_flip(self):
        container = self.fixture['pods']['items'][0]['spec']['containers'][0]
        container['env'] = []
        self.assertIn('has not completed', self.render().stderr)
        container['env'] = [{'name': 'WADDLE_GIT_SHA', 'value': SHA}]
        container['image'] = 'waddle:main'
        self.assertIn('has not completed', self.render().stderr)

    def test_extra_server_pod_blocks_flip_but_unrelated_pod_does_not(self):
        other = self.pod('old')
        self.fixture['pods']['items'].append(other)
        self.assertIn('exactly the desired number', self.render().stderr)
        other['metadata']['labels'] = {'app': 'unrelated'}
        result = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_verified_cutover_allows_repairing_unhealthy_ordinary_rollout(self):
        self.fixture['deployment']['metadata']['annotations'] = {'waddle.social/verified-cutover': SHA}
        self.fixture['deployment']['status'] = {}
        self.fixture['pods'] = {'items': []}
        result = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.fixture['allowedRevisions'] = ['2' * 40]
        self.assertIn('fully observed, ready Deployment', self.render().stderr)

    def test_actual_chart_upgrade_fails_closed_without_live_cluster(self):
        args = ['helm', 'template', 'waddle-server', str(CHART), '--is-upgrade',
                '--set', 'cutoverGuard.enabled=true', '--set', 'cutoverGuard.allowedRevisions[0]=' + SHA,
                '--set', 'updateStrategy.type=RollingUpdate', '--set', 'persistence.enabled=false',
                '--set', 'spicedb.enabled=false', '--set', 'secret.sessionKey=test',
                '--set', 'secret.occupantIdSecret=test-secret-32-bytes-long-enough']
        result = subprocess.run(args, text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cannot find the live Deployment', result.stderr)
        args.remove('--is-upgrade')
        result = subprocess.run(args, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('waddle.social/verified-cutover:', result.stdout)
        args += ['--set', 'updateStrategy.type=Recreate']
        result = subprocess.run(args, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('waddle.social/verified-cutover:', result.stdout)

    def test_old_image_cannot_flip_even_if_recreate_never_reached_cluster(self):
        self.fixture['pods']['items'][0]['spec']['containers'][0]['env'][0]['value'] = '0' * 40
        result = self.render()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('has not completed the Recreate cutover', result.stderr)


if __name__ == '__main__':
    unittest.main()
