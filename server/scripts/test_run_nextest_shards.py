import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT_DIR = Path(__file__).resolve().parent
RUNNER = SCRIPT_DIR / 'run_nextest_shards.py'
REPORTS = ('whole-inventory.json', 'shared-inventory.json', 'whole-coverage.json', 'shared-coverage.json')
JUNIT = ('junit-whole.xml', 'junit-shared.xml')
BINARY = 'waddle-server::clustering_cluster_e2e'
ANCHORS = (
    'waddle-server::sqlx_cancelled_begin cancelled_ordinary_begin_never_reuses_an_open_postgres_transaction',
    'waddle-server::db_lineage_multiprocess two_replicas_same_database_both_ready',
    f'{BINARY} cross_node_resume_live_steal_handshake',
    f'{BINARY} muc_join_routes_to_foreign_room_owner',
    f'{BINARY} groupchat_fanout_reaches_foreign_node_occupant_and_terminalizes',
)


def junit(test_names, outcome=''):
    cases = []
    for test in test_names:
        binary, name = test.split(' ', 1)
        cases.append(f'<testcase name="{name}" classname="{binary}">{outcome}</testcase>')
    return '<testsuites><testsuite>' + ''.join(cases) + '</testsuite></testsuites>'


MOCK_NIX = r'''#!PYTHON
import json, os, pathlib, shutil, subprocess, sys, time
root = pathlib.Path(os.environ['TEST_ROOT'])
diag = root / '.ci/nextest-archive'
store = pathlib.Path(os.environ['TEST_STORE'])
args = sys.argv[1:]
def value(key): return args[args.index(key) + 1]
def state(): return json.loads((diag / 'nix-state.json').read_text())
def drv(i): return str(store / ('a' * 32 + '-waddle-server-test-shard-' + str(i) + '-0.1.0-' + state()['run'] + '.drv'))
def output(i): return str(store / ('b' * 32 + '-waddle-server-test-shard-' + str(i) + '-0.1.0-' + state()['run']))
if os.path.basename(sys.argv[0]) == 'nix-instantiate':
    d = {'source': value('sourcePath'), 'candidate': value('candidateSha'), 'run': value('runId')}
    (diag / 'nix-state.json').write_text(json.dumps(d))
    for i in range(1, 5): print(drv(i))
    sys.exit(0)
if args[:2] == ['store', 'info']:
    print(json.dumps({'url': 'daemon', 'trusted': not bool(os.getenv('UNTRUSTED_DAEMON'))}))
elif args[:2] == ['store', 'add-path']:
    source = store / ('c' * 32 + '-' + value('--name'))
    shutil.copytree(args[-1], source, symlinks=True)
    if os.getenv('TAMPER_SOURCE'): (source / 'flake.nix').write_text('tampered store source\n')
    print(source)
elif args[:2] == ['derivation', 'show']:
    data = state()
    derivations = {}
    for i in range(1, 5):
        env = {'waddleQualificationCandidateSha': data['candidate'], 'waddleQualificationRunId': data['run'],
               'waddleQualificationSource': data['source'], 'preferLocalBuild': '1', 'allowSubstitutes': ''}
        if os.getenv('BAD_DERIVATION') and i == 3: env['waddleQualificationRunId'] = '0' * 64
        derivations[pathlib.Path(drv(i)).name] = {'env': env,
            'inputs': {'srcs': [pathlib.Path(data['source']).name]},
            'outputs': {'out': {'path': pathlib.Path(output(i)).name}}}
    print(json.dumps({'version': 4, 'derivations': derivations}))
elif args[0] == 'path-info':
    if os.getenv('PATH_INFO_ERROR'): sys.exit(24)
    print(json.dumps({path: ({'valid': True} if os.getenv('ALREADY_VALID') and i == 0 else None)
                      for i, path in enumerate(args[-4:])}))
elif args[0] == 'build' and args[-1].endswith('#waddle-ci-sandbox-probe'):
    (diag / 'probe-args.json').write_text(json.dumps(args))
    sys.exit(int(os.getenv('SANDBOX_EXIT', '0')))
elif args[0] == 'build':
    (diag / 'build-args.json').write_text(json.dumps(args))
    if os.getenv('HOLD_BUILD'):
        (diag / 'hold-started').touch()
        while not (diag / 'release-hold').exists(): time.sleep(0.01)
    if os.getenv('CHANGE_CANDIDATE'):
        target = root / '.config/required-cluster-tests.txt'
        target.write_text(target.read_text() + '# changed candidate\n')
        subprocess.run(['git', '-C', str(root.parent), 'add', 'server/.config/required-cluster-tests.txt'], check=True)
        subprocess.run(['git', '-C', str(root.parent), 'commit', '--quiet', '-m', 'change candidate'], check=True)
    if os.getenv('SHARD_EXIT'): sys.exit(int(os.environ['SHARD_EXIT']))
    result = []
    for i in range(1, 5):
        shutil.copytree(diag / 'mock-shards' / ('shard-' + str(i)), output(i), symlinks=True)
        result.append({'drvPath': drv(i), 'outputs': {'out': output(i)}})
    if os.getenv('BAD_RESULT') == 'duplicate': result[1] = result[0]
    if os.getenv('BAD_RESULT') == 'partial': result.pop()
    if os.getenv('BAD_RESULT') == 'mismatch': result[2]['outputs']['out'] = output(1)
    if os.getenv('REPLAY_RESULT'): result = json.loads((diag / 'old-results.json').read_text())
    print(json.dumps(result))
else: sys.exit(99)
'''


class LocalShardTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.server = self.root / 'server'
        self.server.mkdir()
        self.diag = self.server / '.ci/nextest-archive'
        self.store = self.diag / 'mock-store'
        self.store.mkdir(parents=True)
        self.archive = self.store / ('d' * 32 + '-mock-archive')
        (self.archive / 'ci-performance').mkdir(parents=True)
        for name in ('ci-performance/cargo-timing.html', 'partition-coverage.json', 'plan.json'):
            (self.archive / name).write_text('baseline evidence')
        scripts = self.server / 'scripts'
        scripts.mkdir()
        for name in ('check_required_tests.py', 'verify_candidate_snapshot.py', 'qualification-shards.nix'):
            shutil.copyfile(SCRIPT_DIR / name, scripts / name)
        (scripts / 'build-nextest-archive.sh').write_text(
            'printf "%s\\n%s\\n%s\\n" "$PWD" "$0" "$1" > "$TEST_ROOT/.ci/nextest-archive/helper-source"\n'
            'exit_code="${COMPILE_EXIT:-0}"\n'
            'if [ "$exit_code" != 0 ]; then exit "$exit_code"; fi\n'
            'printf "%s\\n" "$TEST_ARCHIVE"\n')
        (self.root / 'flake.nix').write_text('committed source $Format:%H$\n')
        (self.server / '.config').mkdir()
        self.required = self.server / '.config/required-cluster-tests.txt'
        self.required.write_text('\n'.join([*ANCHORS, f'{BINARY} whole_1', f'{BINARY} shared_4']) + '\n')
        for partition in range(1, 5):
            output = self.diag / f'mock-shards/shard-{partition}'
            output.mkdir(parents=True)
            for name in REPORTS:
                (output / name).write_text(f'verified shard {partition}')
            for name in JUNIT:
                (output / name).write_text(junit([*ANCHORS, f'{BINARY} {name[6:-4]}_{partition}']))
        nix = scripts / 'nix'
        nix.write_text(MOCK_NIX.replace('#!PYTHON', f'#!{sys.executable}', 1))
        nix.chmod(0o755)
        (scripts / 'nix-instantiate').symlink_to('nix')
        subprocess.run(['git', '-C', str(self.root), 'init', '--quiet'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'config', 'user.name', 'Fixture'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'config', 'user.email', 'fixture@example.invalid'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'config', 'commit.gpgsign', 'false'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'add', 'flake.nix', 'server/scripts',
                        'server/.config/required-cluster-tests.txt'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'commit', '--quiet', '-m', 'fixture'], check=True)
        self.env = dict(os.environ, PATH=f"{scripts}:{os.environ['PATH']}", TEST_ROOT=str(self.server),
                        TEST_STORE=str(self.store), TEST_ARCHIVE=str(self.archive),
                        PYTHONDONTWRITEBYTECODE='1', PYTHONPATH=str(SCRIPT_DIR))
        # Only this fixture patches the store prefix; production has no override.
        self.runner_args = [sys.executable, '-c', """
import os, pathlib, signal, run_nextest_shards as r
r.STORE_DIR = pathlib.Path(os.environ['TEST_STORE'])
original_unlink = pathlib.Path.unlink
original_rmdir = pathlib.Path.rmdir
def controlled_unlink(path, *args, **kwargs):
    if path.name == 'qualification-evidence.json':
        if os.environ.get('PRE_UNLINK_SIGNAL'):
            os.kill(os.getpid(), getattr(signal, os.environ['PRE_UNLINK_SIGNAL']))
        if os.environ.get('FAIL_UNLINK'):
            raise PermissionError('fixture cannot invalidate sidecar')
    return original_unlink(path, *args, **kwargs)
def controlled_rmdir(path, *args, **kwargs):
    if path.name == '.qualification-run.lock' and (path.parent / 'qualification-evidence.json').exists():
        if os.environ.get('CHECK_INVALIDATION_BEFORE_UNLOCK'):
            raise AssertionError('released lock before invalidating the sidecar')
    return original_rmdir(path, *args, **kwargs)
pathlib.Path.unlink = controlled_unlink
pathlib.Path.rmdir = controlled_rmdir
raise SystemExit(r.main())
"""]

    def run_shards(self, *args):
        return subprocess.run([*self.runner_args, *args], cwd=self.server, env=self.env, text=True, capture_output=True)

    def commit(self, *paths):
        subprocess.run(['git', '-C', str(self.root), 'add', *paths], check=True)
        subprocess.run(['git', '-C', str(self.root), 'commit', '--quiet', '-m', 'change fixture'], check=True)

    def ignore_generated_path(self, path):
        exclude = self.root / '.git/info/exclude'
        with exclude.open('a') as output:
            output.write(f'{path.relative_to(self.root).as_posix()}\n')

    def sidecar(self):
        return self.diag / 'qualification-evidence.json'

    def evidence(self):
        return json.loads(self.sidecar().read_text())

    def hold(self):
        self.env['HOLD_BUILD'] = '1'
        first = subprocess.Popen(self.runner_args, cwd=self.server, env=self.env,
                                 text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        deadline = time.monotonic() + 10
        while not (self.diag / 'hold-started').exists() and first.poll() is None:
            if time.monotonic() >= deadline:
                first.kill()
                self.fail('first run did not reach build barrier')
            time.sleep(0.01)
        self.assertTrue((self.diag / 'hold-started').exists())
        return first

    def release(self, process):
        (self.diag / 'release-hold').touch()
        try:
            return process.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            return process.communicate()

    def test_all_four_isolated_shards_are_required_and_evidence_is_retained(self):
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.diag / 'build-args.json').read_text())
        self.assertEqual(args[-4:], [f"{item['drv_path']}^out" for item in self.evidence()['shards']])
        settings = {args[i + 1]: args[i + 2] for i, arg in enumerate(args) if arg == '--option'}
        self.assertEqual(settings, {'builders': '', 'sandbox': 'true', 'sandbox-fallback': 'false', 'substitute': 'false'})
        self.assertEqual(args[args.index('--store') + 1], 'daemon')
        self.assertEqual(args[args.index('--max-jobs') + 1], '4')
        self.assertEqual(args[args.index('--cores') + 1], '8')
        self.assertIn('--keep-going', args)
        state = json.loads((self.diag / 'nix-state.json').read_text())
        self.assertEqual((self.diag / 'helper-source').read_text().splitlines(),
                         [str(self.server), str(Path(state['source']) / 'server/scripts/build-nextest-archive.sh'),
                          f"path:{state['source']}#waddle-server-test-archive"])
        for partition in range(1, 5):
            for name in REPORTS:
                self.assertEqual((self.diag / f'shard-{partition}/{name}').read_text(), f'verified shard {partition}')
            self.assertFalse((self.diag / f'shard-{partition}').is_symlink())
        for name in ('cargo-timing.html', 'partition-coverage.json', 'plan.json', 'shard-results.json',
                     'executed-tests.txt', 'qualification-evidence.json'):
            self.assertFalse((self.diag / name).is_symlink())
        record = self.evidence()
        self.assertEqual(record['candidate']['worktree'], 'clean')
        self.assertEqual(record['qualification'], 'not_assessed')
        self.assertEqual(len(record['run_id']), 64)
        self.assertIn(record['run_id'], record['candidate']['source_store_path'])
        self.assertEqual(len(record['inputs']['junit_reports']), 8)
        for item in record['inputs']['junit_reports']:
            self.assertEqual(item['sha256'], hashlib.sha256(Path(item['path']).read_bytes()).hexdigest())
            self.assertTrue(item['path'].startswith(str(self.store)))

    def test_diagnostics_root_symlink_is_rejected_before_external_sidecar_is_removed(self):
        external = self.root / 'external-diagnostics'
        self.diag.rename(external)
        sentinel = external / 'qualification-evidence.json'
        sentinel.write_text('previous evidence')
        self.diag.symlink_to(external, target_is_directory=True)

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('diagnostic path component must be a real directory', result.stderr)
        self.assertEqual(sentinel.read_text(), 'previous evidence')
        self.assertFalse((external / '.qualification-run.guard').exists())
        self.assertFalse((external / '.qualification-run.lock').exists())

    def test_diagnostics_parent_symlink_is_rejected_before_external_sidecar_is_removed(self):
        external = self.root / 'external-ci'
        ci_directory = self.server / '.ci'
        ci_directory.rename(external)
        sentinel = external / 'nextest-archive/qualification-evidence.json'
        sentinel.write_text('previous evidence')
        ci_directory.symlink_to(external, target_is_directory=True)

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('diagnostic path component must be a real directory', result.stderr)
        self.assertEqual(sentinel.read_text(), 'previous evidence')
        self.assertFalse((external / 'nextest-archive/.qualification-run.guard').exists())
        self.assertFalse((external / 'nextest-archive/.qualification-run.lock').exists())

    def test_shard_destination_symlink_is_rejected_without_touching_external_files(self):
        external_directory = tempfile.TemporaryDirectory()
        self.addCleanup(external_directory.cleanup)
        external = Path(external_directory.name)
        sentinel = external / 'whole-inventory.json'
        sentinel.write_text('keep this file')
        destination = self.diag / 'shard-1'
        destination.symlink_to(external, target_is_directory=True)
        self.ignore_generated_path(destination)

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('diagnostic path component must be a real directory', result.stderr)
        self.assertEqual(sentinel.read_text(), 'keep this file')
        self.assertFalse(self.sidecar().exists())

    def test_diagnostic_output_symlink_is_rejected_without_touching_external_file(self):
        external_directory = tempfile.TemporaryDirectory()
        self.addCleanup(external_directory.cleanup)
        external = Path(external_directory.name) / 'external-cargo-timing.html'
        external.write_text('keep this file')
        destination = self.diag / 'cargo-timing.html'
        destination.symlink_to(external)
        self.ignore_generated_path(destination)

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('diagnostic output must be a regular file', result.stderr)
        self.assertEqual(external.read_text(), 'keep this file')
        self.assertTrue((self.diag / 'cargo-timing.html').is_symlink())
        self.assertFalse(self.sidecar().exists())

    def test_shard_source_artifact_symlink_is_rejected_without_disclosing_external_file(self):
        external_directory = tempfile.TemporaryDirectory()
        self.addCleanup(external_directory.cleanup)
        external = Path(external_directory.name) / 'whole-inventory.json'
        external.write_text('external secret inventory')
        source = self.diag / 'mock-shards/shard-1/whole-inventory.json'
        source.unlink()
        source.symlink_to(external)

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('source artifact must be a regular file', result.stderr)
        self.assertEqual(external.read_text(), 'external secret inventory')
        self.assertFalse((self.diag / 'shard-1/whole-inventory.json').exists())
        self.assertFalse(self.sidecar().exists())

    def test_archive_source_parent_symlink_is_rejected_without_disclosing_external_file(self):
        external_directory = tempfile.TemporaryDirectory()
        self.addCleanup(external_directory.cleanup)
        external = Path(external_directory.name) / 'cargo-timing.html'
        external.write_text('external archive secret')
        parent = self.archive / 'ci-performance'
        shutil.rmtree(parent)
        parent.symlink_to(external.parent, target_is_directory=True)

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('source artifact parent must be a real directory', result.stderr)
        self.assertEqual(external.read_text(), 'external archive secret')
        self.assertFalse((self.diag / 'cargo-timing.html').exists())
        self.assertFalse(self.sidecar().exists())

    def test_symlinked_sidecar_is_removed_without_touching_external_evidence(self):
        external_directory = tempfile.TemporaryDirectory()
        self.addCleanup(external_directory.cleanup)
        external = Path(external_directory.name) / 'previous-evidence.json'
        external.write_text('previous passing evidence')
        self.sidecar().symlink_to(external)

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('qualification evidence must be a regular file', result.stderr)
        self.assertIn('unsafe sidecar removed and recovery lock retained', result.stderr)
        self.assertFalse(self.sidecar().is_symlink())
        self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
        self.assertEqual(external.read_text(), 'previous passing evidence')

        recovered = self.run_shards('--recover-stale-lock')
        self.assertEqual(recovered.returncode, 0, recovered.stderr)
        self.assertFalse((self.diag / '.qualification-run.lock').exists())

    def test_symlinked_sidecar_is_preserved_when_run_lock_already_exists(self):
        external_directory = tempfile.TemporaryDirectory()
        self.addCleanup(external_directory.cleanup)
        external = Path(external_directory.name) / 'previous-evidence.json'
        external.write_text('previous passing evidence')
        self.sidecar().symlink_to(external)
        lock = self.diag / '.qualification-run.lock'
        lock.mkdir()

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not removed because a qualification run lock already exists', result.stderr)
        self.assertTrue(lock.is_dir())
        self.assertTrue(self.sidecar().is_symlink())
        self.assertEqual(external.read_text(), 'previous passing evidence')

    def test_unsafe_guard_invalidates_old_evidence_and_retains_recovery_lock(self):
        external_directory = tempfile.TemporaryDirectory()
        self.addCleanup(external_directory.cleanup)
        external = Path(external_directory.name) / 'external-guard'
        external.write_text('guard sentinel')
        (self.diag / '.qualification-run.guard').symlink_to(external)
        self.sidecar().write_text('old passing evidence')

        result = self.run_shards()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn('qualification guard must be a regular file', result.stderr)
        self.assertIn('previous evidence invalidated and recovery lock retained', result.stderr)
        self.assertFalse(self.sidecar().exists())
        self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
        self.assertEqual(external.read_text(), 'guard sentinel')

        (self.diag / '.qualification-run.guard').unlink()
        (self.diag / '.qualification-run.guard').touch()
        recovered = self.run_shards('--recover-stale-lock')
        self.assertEqual(recovered.returncode, 0, recovered.stderr)
        self.assertFalse((self.diag / '.qualification-run.lock').exists())

    def test_unsafe_guard_does_not_remove_an_existing_run_lock(self):
        first = self.hold()
        try:
            external_directory = tempfile.TemporaryDirectory()
            self.addCleanup(external_directory.cleanup)
            external = Path(external_directory.name) / 'external-guard'
            external.write_text('guard sentinel')
            self.sidecar().write_text('live holder evidence')
            guard = self.diag / '.qualification-run.guard'
            guard.unlink()
            guard.symlink_to(external)

            result = self.run_shards()

            self.assertNotEqual(result.returncode, 0)
            self.assertIn('not invalidated because a qualification run lock already exists', result.stderr)
            self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
            self.assertEqual(self.sidecar().read_text(), 'live holder evidence')
            self.assertEqual(external.read_text(), 'guard sentinel')
            self.assertIsNone(first.poll())
        finally:
            stdout, stderr = self.release(first)
        self.assertEqual(first.returncode, 0, (stdout, stderr))
        self.assertTrue(self.sidecar().exists())
        self.assertFalse((self.diag / '.qualification-run.lock').exists())

    def test_git_replace_cannot_change_the_tree_attributed_to_candidate(self):
        candidate = subprocess.check_output(['git', '-C', str(self.root), 'rev-parse', 'HEAD'], text=True).strip()
        (self.root / 'flake.nix').write_text('replacement tree B\n')
        self.commit('flake.nix')
        replacement = subprocess.check_output(['git', '-C', str(self.root), 'rev-parse', 'HEAD'], text=True).strip()
        subprocess.run(['git', '-C', str(self.root), 'reset', '--hard', '--quiet', candidate], check=True)
        subprocess.run(['git', '-C', str(self.root), 'replace', candidate, replacement], check=True)
        # Ordinary Git now resolves A's contents to B's tree, proving the
        # replacement is active rather than merely creating an inert ref.
        altered = subprocess.check_output(['git', '-C', str(self.root), 'show', f'{candidate}:flake.nix'], text=True)
        self.assertEqual(altered, 'replacement tree B\n')
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        record = self.evidence()
        self.assertEqual(record['candidate']['commit_sha'], candidate)
        source = Path(record['candidate']['source_store_path'])
        self.assertEqual((source / 'flake.nix').read_text(), 'committed source $Format:%H$\n')

        # The verifier is also safe when called on its own, without inheriting
        # the runner's Git environment: B cannot verify as A.
        with tempfile.TemporaryDirectory() as directory:
            archive = subprocess.check_output(['git', '-C', str(self.root), 'archive', replacement])
            subprocess.run(['tar', '-xf', '-', '-C', directory], input=archive, check=True)
            environment = dict(os.environ)
            environment.pop('GIT_NO_REPLACE_OBJECTS', None)
            checked = subprocess.run([sys.executable, str(SCRIPT_DIR / 'verify_candidate_snapshot.py'),
                                      candidate, directory], cwd=self.root, env=environment,
                                     text=True, capture_output=True)
            self.assertNotEqual(checked.returncode, 0)
            self.assertIn('content differs from Git tree: flake.nix', checked.stderr)

    def test_interruption_before_invalidation_requires_safe_recovery(self):
        for signum in ('SIGTERM', 'SIGKILL'):
            with self.subTest(signal=signum):
                self.sidecar().write_text('old passing evidence')
                self.env['PRE_UNLINK_SIGNAL'] = signum
                interrupted_run = self.run_shards()
                self.assertNotEqual(interrupted_run.returncode, 0)
                self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
                self.assertEqual(self.sidecar().read_text(), 'old passing evidence')
                del self.env['PRE_UNLINK_SIGNAL']
                self.assertIn('cannot acquire nextest qualification run lock', self.run_shards().stderr)
                self.env['CHECK_INVALIDATION_BEFORE_UNLOCK'] = '1'
                recovered = self.run_shards('--recover-stale-lock')
                self.assertEqual(recovered.returncode, 0, recovered.stderr)
                self.assertFalse(self.sidecar().exists())
                self.assertFalse((self.diag / '.qualification-run.lock').exists())
                del self.env['CHECK_INVALIDATION_BEFORE_UNLOCK']

    def test_failed_invalidation_retains_lock_until_recovery(self):
        self.sidecar().write_text('old passing evidence')
        self.env['FAIL_UNLINK'] = '1'
        self.assertNotEqual(self.run_shards().returncode, 0)
        self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
        self.assertEqual(self.sidecar().read_text(), 'old passing evidence')
        self.assertNotEqual(self.run_shards('--recover-stale-lock').returncode, 0)
        self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
        del self.env['FAIL_UNLINK']
        self.env['CHECK_INVALIDATION_BEFORE_UNLOCK'] = '1'
        self.assertEqual(self.run_shards('--recover-stale-lock').returncode, 0)
        self.assertFalse(self.sidecar().exists())
        self.assertFalse((self.diag / '.qualification-run.lock').exists())

    def test_recovery_cannot_unlock_a_live_runner(self):
        first = self.hold()
        try:
            recovered = self.run_shards('--recover-stale-lock')
            self.assertNotEqual(recovered.returncode, 0)
            self.assertIn('another process holds the guard', recovered.stderr)
            self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
        finally:
            stdout, stderr = self.release(first)
        self.assertEqual(first.returncode, 0, (stdout, stderr))
        self.assertTrue(self.sidecar().exists())

    def test_new_run_nonce_changes_source_and_output_identities(self):
        first = self.run_shards()
        self.assertEqual(first.returncode, 0, first.stderr)
        old = self.evidence()
        second = self.run_shards()
        self.assertEqual(second.returncode, 0, second.stderr)
        new = self.evidence()
        self.assertNotEqual(old['run_id'], new['run_id'])
        self.assertNotEqual(old['candidate']['source_store_path'], new['candidate']['source_store_path'])
        self.assertTrue(set(x['output_path'] for x in old['shards']).isdisjoint(x['output_path'] for x in new['shards']))
        self.assertTrue(set(x['drv_path'] for x in old['shards']).isdisjoint(x['drv_path'] for x in new['shards']))

    def test_old_build_results_cannot_attest_a_new_candidate(self):
        self.assertEqual(self.run_shards().returncode, 0)
        shutil.copyfile(self.diag / 'shard-results.json', self.diag / 'old-results.json')
        self.required.write_text(self.required.read_text() + '# new candidate\n')
        self.commit('server/.config/required-cluster-tests.txt')
        self.env['REPLAY_RESULT'] = '1'
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('do not match', result.stderr)
        self.assertFalse(self.sidecar().exists())

    def test_candidate_change_during_build_cannot_write_evidence(self):
        self.env['CHANGE_CANDIDATE'] = '1'
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('candidate SHA mismatch', result.stderr)
        self.assertFalse(self.sidecar().exists())

    def test_already_valid_outputs_and_undetermined_store_state_fail_before_build(self):
        for setting in ('ALREADY_VALID', 'PATH_INFO_ERROR', 'UNTRUSTED_DAEMON', 'BAD_DERIVATION'):
            with self.subTest(setting=setting):
                self.env[setting] = '1'
                result = self.run_shards()
                self.assertNotEqual(result.returncode, 0, setting)
                self.assertFalse((self.diag / 'build-args.json').exists())
                self.assertFalse(self.sidecar().exists())
                del self.env[setting]

    def test_partial_duplicate_and_mismatched_build_results_fail_closed(self):
        for setting in ('partial', 'duplicate', 'mismatch'):
            with self.subTest(setting=setting):
                self.env['BAD_RESULT'] = setting
                result = self.run_shards()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.sidecar().exists())

    def test_archive_export_transformations_fail_before_nix_builds(self):
        for attribute in ('export-ignore', 'export-subst'):
            with self.subTest(attribute=attribute):
                (self.root / '.gitattributes').write_text(f'flake.nix {attribute}\n')
                self.commit('.gitattributes')
                result = self.run_shards()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('candidate source snapshot:', result.stderr)
                self.assertFalse((self.diag / 'probe-args.json').exists())
                self.assertFalse(self.sidecar().exists())

    def test_gitlinks_and_modified_materialized_source_fail_closed(self):
        self.env['TAMPER_SOURCE'] = '1'
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('content differs from Git tree: flake.nix', result.stderr)
        del self.env['TAMPER_SOURCE']
        sha = subprocess.check_output(['git', '-C', str(self.root), 'rev-parse', 'HEAD'], text=True).strip()
        subprocess.run(['git', '-C', str(self.root), 'update-index', '--add', '--cacheinfo', f'160000,{sha},module'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'commit', '--quiet', '-m', 'gitlink fixture'], check=True)
        (self.root / 'module').mkdir()
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('unsupported Git tree entry: module', result.stderr)

    def test_source_symlinks_cannot_escape_immutable_store_tree(self):
        link = self.root / 'source-link'
        link.symlink_to('flake.nix')
        self.commit('source-link')
        self.assertEqual(self.run_shards().returncode, 0)
        link.unlink()
        link.symlink_to('../outside-source')
        self.commit('source-link')
        result = self.run_shards()
        self.assertIn('symlink escapes source:', result.stderr)
        self.assertFalse(self.sidecar().exists())

    def test_chained_symlinks_resolve_components_and_reject_loops(self):
        directory = self.root / 'dir'
        directory.mkdir()
        (directory / 's').symlink_to('.')
        link = self.root / 'source-link'
        link.symlink_to('dir/s/../flake.nix')
        self.commit('dir/s', 'source-link')
        self.assertEqual(self.run_shards().returncode, 0)
        link.unlink()
        link.symlink_to('dir/s/../../outside')
        self.commit('source-link')
        self.assertIn('symlink escapes source:', self.run_shards().stderr)
        link.unlink()
        link.symlink_to('loop')
        (self.root / 'loop').symlink_to('source-link')
        self.commit('source-link', 'loop')
        self.assertIn('symlink loop or too many symlinks:', self.run_shards().stderr)
        self.assertFalse(self.sidecar().exists())

    def test_overlapping_invocations_cannot_replace_reports_or_sidecar(self):
        first = self.hold()
        try:
            self.sidecar().write_text('sentinel')
            second = self.run_shards()
            self.assertIn('cannot acquire nextest qualification run lock', second.stderr)
            self.assertEqual(self.sidecar().read_text(), 'sentinel')
            self.sidecar().unlink()
        finally:
            stdout, stderr = self.release(first)
        self.assertEqual(first.returncode, 0, (stdout, stderr))
        self.assertFalse((self.diag / '.qualification-run.lock').exists())
        self.assertTrue(self.sidecar().exists())

    def test_interrupted_invocation_keeps_lock_for_manual_recovery(self):
        first = self.hold()
        try:
            first.terminate()
        finally:
            stdout, stderr = self.release(first)
        self.assertEqual(first.returncode, 143, (stdout, stderr))
        self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
        self.assertIn('cannot acquire nextest qualification run lock', self.run_shards().stderr)
        self.assertFalse(self.sidecar().exists())

    def test_sigkill_retains_lock_and_no_sidecar(self):
        first = self.hold()
        first.kill()
        self.release(first)
        self.assertTrue((self.diag / '.qualification-run.lock').is_dir())
        self.assertFalse(self.sidecar().exists())
        self.assertIn('cannot acquire nextest qualification run lock', self.run_shards().stderr)

    def test_executed_test_names_from_every_shard_are_emitted(self):
        result = self.run_shards()
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = sorted([*ANCHORS, *(f'{BINARY} {selection}_{partition}'
                           for selection in ('whole', 'shared') for partition in range(1, 5))])
        self.assertEqual((self.diag / 'executed-tests.txt').read_text().splitlines(), expected)
        self.assertEqual(self.evidence()['required_test_gate']['executed_inventory'], expected)

    def test_required_test_that_did_not_run_fails_the_lane(self):
        self.required.write_text('\n'.join([*ANCHORS, f'{BINARY} returned_early_or_filtered']) + '\n')
        self.commit('server/.config/required-cluster-tests.txt')
        result = self.run_shards()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f'required test did not run: {BINARY} returned_early_or_filtered', result.stderr)
        self.assertFalse(self.sidecar().exists())

    def test_missing_shard_junit_report_cannot_report_success(self):
        (self.diag / 'mock-shards/shard-2/junit-shared.xml').unlink()
        self.assertNotEqual(self.run_shards().returncode, 0)
        self.assertFalse(self.sidecar().exists())

    def test_failed_shard_fails_the_job_and_preserves_compiler_diagnostics(self):
        self.sidecar().write_text('{"old": true}')
        self.env['SHARD_EXIT'] = '17'
        self.assertEqual(self.run_shards().returncode, 17)
        self.assertTrue((self.diag / 'cargo-timing.html').is_file())
        self.assertFalse(self.sidecar().exists())

    def test_unavailable_sandbox_fails_before_compilation(self):
        self.sidecar().write_text('{"old": true}')
        self.env['SANDBOX_EXIT'] = '23'
        self.env['COMPILE_EXIT'] = '19'
        self.assertEqual(self.run_shards().returncode, 23)
        self.assertFalse((self.diag / 'build-args.json').exists())
        self.assertFalse(self.sidecar().exists())

    def test_failed_compile_cannot_start_shards(self):
        self.sidecar().write_text('{"old": true}')
        self.env['COMPILE_EXIT'] = '19'
        self.assertEqual(self.run_shards().returncode, 19)
        self.assertFalse((self.diag / 'build-args.json').exists())
        self.assertFalse(self.sidecar().exists())

    def test_dirty_candidate_fails_before_nix_and_removes_old_sidecar(self):
        self.sidecar().write_text('{"old": true}')
        self.required.write_text(self.required.read_text() + '# dirty\n')
        result = self.run_shards()
        self.assertIn('candidate worktree is dirty', result.stderr)
        self.assertFalse((self.diag / 'probe-args.json').exists())
        self.assertFalse(self.sidecar().exists())

    def test_symlinked_junit_cannot_escape_the_store_output(self):
        report = self.diag / 'mock-shards/shard-1/junit-whole.xml'
        old = self.diag / 'old-report.xml'
        old.write_text(report.read_text())
        report.unlink()
        report.symlink_to(old)
        result = self.run_shards()
        self.assertIn('source artifact must be a regular file', result.stderr)
        self.assertFalse(self.sidecar().exists())

    def test_staged_and_untracked_candidate_changes_fail_closed(self):
        self.sidecar().write_text('old')
        self.required.write_text(self.required.read_text() + '# staged\n')
        subprocess.run(['git', '-C', str(self.root), 'add', 'server/.config/required-cluster-tests.txt'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'checkout', '--', 'server/.config/required-cluster-tests.txt'], check=True)
        self.assertIn('candidate index is dirty', self.run_shards().stderr)
        self.assertFalse(self.sidecar().exists())
        subprocess.run(['git', '-C', str(self.root), 'reset', '--hard', '--quiet', 'HEAD'], check=True)
        (self.server / 'untracked.rs').write_text('untracked')
        self.assertIn('untracked files outside', self.run_shards().stderr)
        self.assertFalse(self.sidecar().exists())

    def test_missing_shard_coverage_cannot_report_success(self):
        (self.diag / 'mock-shards/shard-4/shared-coverage.json').unlink()
        self.assertNotEqual(self.run_shards().returncode, 0)
        self.assertFalse(self.sidecar().exists())

    def test_anchor_inventory_is_required(self):
        self.required.write_text('\n'.join(ANCHORS[1:]) + '\n')
        self.commit('server/.config/required-cluster-tests.txt')
        self.assertIn('prerequisite anchor', self.run_shards().stderr)
        self.assertFalse(self.sidecar().exists())

    def test_skipped_failed_and_malformed_results_cannot_attest(self):
        target = self.diag / 'mock-shards/shard-1/junit-whole.xml'
        for report in (junit([ANCHORS[0]], '<skipped/>'), junit([ANCHORS[0]], '<failure/>'), '<testsuites><'):
            with self.subTest(report=report):
                target.write_text(report)
                self.assertNotEqual(self.run_shards().returncode, 0)
                self.assertFalse(self.sidecar().exists())


if __name__ == '__main__':
    unittest.main()
