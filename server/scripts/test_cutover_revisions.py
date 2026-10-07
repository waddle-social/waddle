import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from cutover_revisions import SERVER_INPUTS

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

    def test_server_docs_do_not_require_an_image_that_was_never_built(self):
        self.commit('RollingUpdate', 'v1')
        cutover = self.commit('Recreate', 'v2')
        docs = self.root / 'server/docs/operations/cutover.md'
        docs.parent.mkdir(parents=True)
        docs.write_text('Document the completed cutover')
        documentation = self.commit()
        flip = self.commit('RollingUpdate')
        self.assertEqual(self.revisions(), [cutover, documentation, flip])

    def test_agent_guidance_does_not_require_an_image_that_was_never_built(self):
        cutover = self.commit('Recreate', 'v2')
        (self.root / 'server/AGENTS.md').write_text('Server guidance')
        guidance = self.commit()
        flip = self.commit('RollingUpdate')
        self.assertEqual(self.revisions(), [cutover, guidance, flip])

    def test_unpublished_docs_and_guidance_can_accompany_the_flip(self):
        cutover = self.commit('Recreate', 'v2')
        (self.root / 'server/docs').mkdir()
        (self.root / 'server/docs/cutover.md').write_text('Cutover runbook')
        (self.root / 'server/AGENTS.md').write_text('Server guidance')
        flip = self.commit('RollingUpdate')
        self.assertEqual(self.revisions(), [cutover, flip])

    def test_published_build_inputs_advance_the_floor_and_cannot_accompany_the_flip(self):
        for path in (
            'server/Cargo.lock', 'server/.cargo/config.toml', 'server/env.cue',
            'server/extensions/example/env.cue', 'server/schema/migration.sql',
            'server/charts/waddle-server/values.yaml', 'server/scripts/build.sh',
            'server/wit/extension.wit', 'flake.nix', 'flake.lock',
        ):
            with self.subTest(path=path):
                self.commit('Recreate', 'v2')
                source = self.root / path
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text('Build change')
                required = self.commit()
                flip = self.commit('RollingUpdate')
                self.assertEqual(self.revisions(), [required, flip])
                self.commit('Recreate')
                source.write_text('Another build change')
                self.commit('RollingUpdate')
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), '--repository', str(self.root)],
                    capture_output=True, text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('server changes in the RollingUpdate flip', result.stderr)

    def test_server_inputs_match_publisher_triggers(self):
        workflow = SCRIPT.resolve().parents[2] / '.github/workflows/waddle-server-default.yml'
        paths = json.loads(subprocess.check_output(
            ['yq', '-o=json', '.on.push.paths', str(workflow)], text=True,
        ))
        self.assertFalse(any(path.startswith('!') for path in paths),
                         'publisher exclusions require updating the cutover input policy')
        publisher_paths = [path for path in paths
                           if path.startswith('server/') or path in ('flake.nix', 'flake.lock')]
        guard_paths = [path.removeprefix(':(top,glob)') for path in SERVER_INPUTS]
        # Exercise both policies against representative files, including inputs
        # removed from either side and paths that intentionally do not publish.
        patterns = set(publisher_paths + guard_paths)
        samples = {'server/docs/cutover.md', 'server/AGENTS.md', 'server/CLAUDE.md',
                   'server/Justfile', 'server/docker-compose.yml', 'server/wasm-pkg/README.md'}
        for pattern in patterns:
            if pattern.endswith('/**'):
                if pattern[:-3] in patterns:
                    continue  # Generated file/** duplicates the exact file input.
                pattern = pattern[:-3] + '/fixture.txt'
            samples.add(pattern.replace('**', 'nested').replace('*', 'sample'))
        for sample in samples:
            source = self.root / sample
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text('fixture')
        self.git('add', '.')
        published = self.git('ls-files', '--',
                             *[f':(top,glob){path}' for path in publisher_paths])
        guarded = self.git('ls-files', '--', *SERVER_INPUTS)
        self.assertEqual(set(guarded.splitlines()), set(published.splitlines()))

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
