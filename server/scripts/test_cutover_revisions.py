import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('cutover_revisions.py')
MANIFEST = 'infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml'


class CutoverRevisionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'test@example.test')
        self.git('config', 'user.name', 'Test')
        (self.root / MANIFEST).parent.mkdir(parents=True)
        (self.root / 'server/crates').mkdir(parents=True)

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.root), *args], text=True).strip()

    def commit(self, strategy=None, wire=None):
        if strategy:
            (self.root / MANIFEST).write_text(f'spec:\n  values:\n    updateStrategy:\n      type: {strategy}\n')
        if wire:
            (self.root / 'server/crates/wire.rs').write_text(wire)
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')
        return self.git('rev-parse', 'HEAD')

    def revisions(self):
        result = subprocess.run([sys.executable, str(SCRIPT), '--repository', str(self.root)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_recreate_publishing_does_not_require_a_completed_cutover(self):
        self.commit('Recreate', 'v2')
        self.assertEqual(self.revisions(), [])

    def test_later_wire_change_during_recreate_advances_the_required_revision(self):
        self.commit('RollingUpdate', 'v1')
        self.commit('Recreate', 'v2')
        cutover = self.commit(wire='v3')
        flip = self.commit('RollingUpdate')
        self.assertEqual(self.revisions(), [cutover, flip])

    def test_unrelated_commit_does_not_require_an_image_that_was_never_built(self):
        self.commit('RollingUpdate', 'v1')
        cutover = self.commit('Recreate', 'v2')
        (self.root / 'unrelated.txt').write_text('docs only')
        unrelated = self.commit()
        flip = self.commit('RollingUpdate')
        self.assertEqual(self.revisions(), [cutover, unrelated, flip])

    def test_latest_cutover_replaces_previous_accepted_images(self):
        self.commit('Recreate', 'v1')
        self.commit('RollingUpdate')
        cutover = self.commit('Recreate', 'v2')
        flip = self.commit('RollingUpdate')
        self.assertEqual(self.revisions(), [cutover, flip])

    def test_shallow_history_fails_closed(self):
        self.commit('Recreate', 'v1')
        self.commit('RollingUpdate')
        shallow = self.root / 'shallow'
        subprocess.run(['git', 'clone', '-q', '--depth=1', self.root.as_uri(), str(shallow)], check=True)
        result = subprocess.run([sys.executable, str(SCRIPT), '--repository', str(shallow)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('complete git history', result.stderr)

    def test_no_cutover_history_fails_closed(self):
        self.commit('RollingUpdate', 'v1')
        result = subprocess.run([sys.executable, str(SCRIPT), '--repository', str(self.root)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('no Recreate cutover', result.stderr)

    def test_premature_flip_cannot_accept_the_previous_image(self):
        old = self.commit('RollingUpdate', 'v1')
        cutover = self.commit('Recreate', 'v2')
        flip = self.commit('RollingUpdate')
        revisions = self.revisions()
        self.assertEqual(revisions, [cutover, flip])
        self.assertNotIn(old, revisions)

    def test_server_change_in_flip_commit_fails_closed(self):
        self.commit('Recreate', 'v2')
        self.commit('RollingUpdate', 'v3')
        result = subprocess.run(
            [sys.executable, str(SCRIPT), '--repository', str(self.root)],
            capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('server changes in the RollingUpdate flip', result.stderr)


if __name__ == '__main__':
    unittest.main()
