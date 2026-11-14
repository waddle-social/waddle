#!/usr/bin/env python3
"""Run the required lane and write evidence only for this fresh local Nix build."""
from __future__ import annotations

import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
from contextlib import contextmanager
import re
import secrets
import shutil
import stat
import signal
import subprocess
import sys
import tempfile

class InputError(Exception):
    pass


class UnsafeQualificationGuard(InputError):
    pass


class UnsafeDiagnosticSidecar(InputError):
    pass


STORE_DIR = Path('/nix/store')
STORE_NAME = re.compile(r'^[0123456789abcdfghijklmnpqrsvwxyz]{32}-[^/\s]+$')
DIAGNOSTICS = Path('server/.ci/nextest-archive')
ARTIFACTS = ('whole-inventory.json', 'shared-inventory.json', 'whole-coverage.json',
             'shared-coverage.json', 'junit-whole.xml', 'junit-shared.xml')


class CommandError(InputError):
    def __init__(self, message, returncode):
        super().__init__(message)
        self.returncode = returncode


class InterruptedRun(BaseException):
    def __init__(self, signum: int):
        self.signum = signum


def interrupted(signum, _frame):
    raise InterruptedRun(signum)


def git_environment():
    return {**os.environ, 'GIT_NO_REPLACE_OBJECTS': '1'}


def command(args, *, cwd=None, capture=True):
    try:
        return subprocess.run(args, cwd=cwd, check=True, text=True,
                              env=git_environment() if args[0] == 'git' else None,
                              stdout=subprocess.PIPE if capture else None).stdout
    except subprocess.CalledProcessError as error:
        raise CommandError(f'command failed: {args[0]}: {error}', error.returncode) from error
    except OSError as error:
        raise InputError(f'command failed: {args[0]}: {error}') from error


def json_data(contents):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise InputError(f'duplicate JSON key: {key}')
            result[key] = value
        return result
    try:
        return json.loads(contents, object_pairs_hook=unique)
    except (TypeError, json.JSONDecodeError) as error:
        raise InputError(f'invalid Nix JSON: {error}') from error


def store_path(value, *, derivation=False):
    if not isinstance(value, str):
        raise InputError('invalid Nix store path')
    path = Path(value)
    if path.parent != STORE_DIR or not STORE_NAME.fullmatch(path.name):
        raise InputError(f'invalid Nix store path: {value}')
    if derivation != path.name.endswith('.drv'):
        raise InputError(f'unexpected Nix store path type: {value}')
    return path


def git(root, *args):
    return command(['git', '-C', str(root), *args])


def check_candidate(root, expected=None):
    actual = git(root, 'rev-parse', '--verify', 'HEAD^{commit}').strip()
    if not re.fullmatch(r'[0-9a-f]{40}', actual):
        raise InputError('candidate SHA must be a full lowercase 40-character Git SHA')
    if expected is not None and actual != expected:
        raise InputError(f'candidate SHA mismatch: expected {expected}, found {actual}')
    for args, label in ((['diff', '--cached', '--quiet', 'HEAD', '--'], 'index'),
                        (['diff', '--quiet', '--'], 'worktree')):
        try:
            result = subprocess.run(['git', '-C', str(root), *args], check=False, env=git_environment())
        except OSError as error:
            raise InputError(f'cannot inspect candidate {label}: {error}') from error
        if result.returncode != 0:
            raise InputError(f'candidate {label} is dirty or cannot be inspected')
    if git(root, 'ls-files', '--others', '--exclude-standard', '-z', '--', '.',
           ':(exclude)server/.ci/nextest-archive/**'):
        raise InputError('candidate worktree has untracked files outside the nextest evidence directory')
    return actual


def derivation_identities(drvs, source, candidate, run_id):
    if len(drvs) != 4 or len(set(drvs)) != 4:
        raise InputError('expected four unique shard derivations')
    for path in drvs:
        store_path(path, derivation=True)
    data = json_data(command(['nix', 'derivation', 'show', '--store', 'daemon', *drvs]))
    # Nix 2.34 emits its versioned derivation schema; older releases use a
    # map keyed by full store paths. Accept only known, unambiguous shapes.
    if isinstance(data, dict) and 'derivations' in data:
        if data.get('version') != 4 or set(data) != {'version', 'derivations'}:
            raise InputError('unsupported Nix derivation JSON version')
        values = data['derivations']
        if not isinstance(values, dict) or set(values) != {Path(p).name for p in drvs}:
            raise InputError('Nix returned unexpected derivations')
        entries = [values[Path(p).name] for p in drvs]
        versioned = True
    elif isinstance(data, dict) and set(data) == set(drvs):
        entries = [data[p] for p in drvs]
        versioned = False
    else:
        raise InputError('Nix returned unexpected derivations')
    identities = []
    for partition, (drv, entry) in enumerate(zip(drvs, entries), 1):
        try:
            env = entry['env']
            attrs = {
                'waddleQualificationCandidateSha': candidate,
                'waddleQualificationRunId': run_id,
                'waddleQualificationSource': str(source),
                'preferLocalBuild': '1',
                'allowSubstitutes': '',
            }
            if any(env.get(key) != value for key, value in attrs.items()):
                raise InputError(f'shard {partition} is missing qualification attributes')
            sources = entry['inputs']['srcs'] if versioned else entry['inputSrcs']
            expected_source = source.name if versioned else str(source)
            if expected_source not in sources:
                raise InputError(f'shard {partition} does not reference the run source')
            outputs = entry['outputs']
            if set(outputs) != {'out'}:
                raise InputError(f'shard {partition} must have exactly one output')
            output = outputs['out']['path']
            if versioned:
                output = str(STORE_DIR / output)
            output = str(store_path(output))
        except (KeyError, TypeError) as error:
            raise InputError(f'invalid shard {partition} derivation metadata') from error
        if f'waddle-server-test-shard-{partition}-' not in Path(drv).name:
            raise InputError(f'shard derivation order mismatch at partition {partition}')
        identities.append({'partition': partition, 'drv_path': drv, 'output_path': output})
    if len({item['output_path'] for item in identities}) != 4:
        raise InputError('shard outputs are not unique')
    return identities


def reject_existing_outputs(identities):
    paths = [item['output_path'] for item in identities]
    info = json_data(command(['nix', 'path-info', '--store', 'daemon', '--offline',
                              '--json', '--json-format', '1', *paths]))
    if not isinstance(info, dict) or set(info) != set(paths):
        raise InputError('cannot determine whether all shard outputs already exist')
    if any(info[path] is not None for path in paths):
        raise InputError('a run-specific shard output is already valid')


def validate_build_results(identities, contents):
    data = json_data(contents)
    if not isinstance(data, list) or len(data) != 4:
        raise InputError('Nix did not return exactly four shard build results')
    expected = {(item['drv_path'], item['output_path']) for item in identities}
    actual = []
    for item in data:
        try:
            outputs = item['outputs']
            if set(outputs) != {'out'}:
                raise InputError('shard build returned unexpected outputs')
            actual.append((item['drvPath'], outputs['out']))
        except (KeyError, TypeError) as error:
            raise InputError('invalid shard build result') from error
    if len(set(actual)) != 4 or set(actual) != expected:
        raise InputError('shard build results do not match the expected derivations and outputs')


def digest(path):
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError as error:
        raise InputError(f'{path}: cannot hash evidence input') from error


def ensure_real_directory(path, *, create=False):
    try:
        try:
            info = path.lstat()
        except FileNotFoundError:
            if not create:
                raise InputError(f'{path}: diagnostic directory is missing')
            try:
                path.mkdir()
            except FileExistsError:
                pass
            info = path.lstat()
    except OSError as error:
        raise InputError(f'{path}: cannot inspect diagnostic directory') from error
    if not stat.S_ISDIR(info.st_mode):
        raise InputError(f'{path}: diagnostic path component must be a real directory')
    return path


def prepare_diagnostics(root):
    path = root
    for index, component in enumerate(DIAGNOSTICS.parts):
        path /= component
        ensure_real_directory(path, create=index > 0)
    return path


def ensure_regular_output(path):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise InputError(f'{path}: cannot inspect diagnostic output') from error
    if not stat.S_ISREG(info.st_mode):
        raise InputError(f'{path}: diagnostic output must be a regular file')


def ensure_safe_sidecar(path):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise InputError(f'{path}: cannot inspect diagnostic sidecar') from error
    if stat.S_ISLNK(info.st_mode):
        raise UnsafeDiagnosticSidecar(f'{path}: qualification evidence must be a regular file')
    if not stat.S_ISREG(info.st_mode):
        raise InputError(f'{path}: diagnostic sidecar must be a regular file')


def atomic_write_text(path, contents, *, prefix=None):
    ensure_regular_output(path)
    name = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=path.parent,
                                         prefix=prefix or f'.{path.name}-', delete=False) as output:
            name = output.name
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
        ensure_regular_output(path)
        os.replace(name, path)
    except OSError as error:
        raise InputError(f'{path}: cannot write diagnostic output') from error
    finally:
        if name and os.path.exists(name):
            os.unlink(name)


def open_store_input(store_root, output_root, relative):
    path = output_root / relative
    if output_root.parent != store_root or not STORE_NAME.fullmatch(output_root.name):
        raise InputError(f'{output_root}: source artifact root is not a direct Nix store output')
    if relative.is_absolute() or not relative.parts or any(part in ('.', '..') for part in relative.parts):
        raise InputError(f'{path}: invalid source artifact path')
    directory_flags = os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0) | getattr(os, 'O_NOFOLLOW', 0)
    file_flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
    descriptors = []
    try:
        store_descriptor = os.open(store_root, directory_flags)
        descriptors.append(store_descriptor)
        if not stat.S_ISDIR(os.fstat(store_descriptor).st_mode):
            raise InputError(f'{store_root}: Nix store root must be a real directory')

        info = os.stat(output_root.name, dir_fd=store_descriptor, follow_symlinks=False)
        if not stat.S_ISDIR(info.st_mode):
            raise InputError(f'{output_root}: source artifact root must be a real directory')
        directory_descriptor = os.open(output_root.name, directory_flags, dir_fd=store_descriptor)
        descriptors.append(directory_descriptor)
        opened_info = os.fstat(directory_descriptor)
        same_directory = (info.st_dev, info.st_ino) == (opened_info.st_dev, opened_info.st_ino)
        if not stat.S_ISDIR(opened_info.st_mode) or not same_directory:
            raise InputError(f'{output_root}: source artifact root must be a real directory')

        for component in relative.parts[:-1]:
            info = os.stat(component, dir_fd=directory_descriptor, follow_symlinks=False)
            if not stat.S_ISDIR(info.st_mode):
                raise InputError(f'{output_root / relative}: source artifact parent must be a real directory')
            child_descriptor = os.open(component, directory_flags, dir_fd=directory_descriptor)
            descriptors.append(child_descriptor)
            opened_info = os.fstat(child_descriptor)
            same_directory = (info.st_dev, info.st_ino) == (opened_info.st_dev, opened_info.st_ino)
            if not stat.S_ISDIR(opened_info.st_mode) or not same_directory:
                raise InputError(f'{output_root / relative}: source artifact parent must be a real directory')
            os.close(directory_descriptor)
            descriptors.remove(directory_descriptor)
            directory_descriptor = child_descriptor

        leaf = relative.parts[-1]
        info = os.stat(leaf, dir_fd=directory_descriptor, follow_symlinks=False)
        if not stat.S_ISREG(info.st_mode):
            raise InputError(f'{path}: source artifact must be a regular file')
        descriptor = os.open(leaf, file_flags, dir_fd=directory_descriptor)
        descriptors.append(descriptor)
        opened_info = os.fstat(descriptor)
        same_file = (info.st_dev, info.st_ino) == (opened_info.st_dev, opened_info.st_ino)
        if not stat.S_ISREG(opened_info.st_mode) or not same_file:
            raise InputError(f'{path}: source artifact must be a regular file')
        contents = os.fdopen(descriptor, 'rb')
        descriptors.remove(descriptor)
        return contents
    except OSError as error:
        raise InputError(f'{path}: cannot open source artifact without following symlinks') from error
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def atomic_copy(store_root, source_root, relative, destination):
    ensure_regular_output(destination)
    name = None
    try:
        with tempfile.NamedTemporaryFile(mode='wb', dir=destination.parent,
                                         prefix=f'.{destination.name}-', delete=False) as output:
            name = output.name
            with open_store_input(store_root, source_root, relative) as contents:
                shutil.copyfileobj(contents, output)
            output.flush()
            os.fsync(output.fileno())
        ensure_regular_output(destination)
        os.replace(name, destination)
    except OSError as error:
        raise InputError(f'{destination}: cannot copy diagnostic input') from error
    finally:
        if name and os.path.exists(name):
            os.unlink(name)


def load_snapshot_gate(source):
    path = source / 'server/scripts/check_required_tests.py'
    spec = importlib.util.spec_from_file_location('qualification_snapshot_gate', path)
    if spec is None or spec.loader is None:
        raise InputError('cannot load the committed required-test gate')
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def build_record(candidate, run_id, source, identities, required_path, diagnostics, gate):
    reports = [Path(item['output_path']) / f'junit-{selection}.xml'
               for item in identities for selection in ('whole', 'shared')]
    for report in reports:
        if report.parent.is_symlink() or not report.parent.is_dir():
            raise InputError(f'{report.parent}: shard output is not a store directory')
        try:
            if not stat.S_ISREG(report.lstat().st_mode):
                raise InputError(f'{report}: JUnit report is not a regular store file')
        except OSError as error:
            raise InputError(f'{report}: cannot inspect JUnit report') from error
    try:
        required, required_digest = gate.parse_required(required_path)
        gate.validate_anchor_inventory(required)
        executed, failed, skipped, report_digests = gate.executed_tests(reports)
    except gate.InputError as error:
        raise InputError(str(error)) from error
    atomic_write_text(diagnostics / 'executed-tests.txt',
                      ''.join(f'{test}\n' for test in sorted(executed)))
    missing = [test for test in required if test not in executed or test in skipped]
    failing = [test for test in required if test in failed]
    if missing or failing:
        gate.report_failure(missing, failing, executed)
        raise InputError(f'{len(missing) + len(failing)} of {len(required)} required tests did not pass')
    if digest(required_path) != required_digest:
        raise InputError('required-test inventory changed after parsing')
    for report in reports:
        if digest(report) != report_digests[report.resolve()]:
            raise InputError(f'{report}: report changed after parsing')
    outcomes = gate.required_test_outcomes(required, executed, failed, skipped)
    anchors = {name: {'outcome': 'passed', 'tests': list(tests)}
               for name, tests in gate.PREREQUISITE_ANCHORS.items()}
    return {
        'schema_version': 2,
        'lane_id': gate.LANE_ID,
        'candidate': {'commit_sha': candidate, 'worktree': 'clean', 'source_store_path': str(source)},
        'run_id': run_id,
        'shards': identities,
        'inputs': {
            'required_tests': {'path': 'server/.config/required-cluster-tests.txt', 'sha256': required_digest},
            'junit_reports': [{'path': str(path), 'sha256': report_digests[path.resolve()]} for path in reports],
        },
        'required_test_gate': {
            'outcome': 'passed', 'required_count': len(required), 'executed_count': len(executed),
            'required_tests': outcomes, 'executed_inventory': [str(test) for test in sorted(executed)],
        },
        'prerequisite_anchors': anchors,
        'qualification': 'not_assessed',
        'unverified': {name: 'unverified' for name in
                       ('image', 'topology', 'resource_enforcement', 'workload', 'faults', 'deployment')},
    }


def atomic_write(path, record):
    atomic_write_text(path, json.dumps(record, indent=2, sort_keys=True) + '\n',
                      prefix='.qualification-evidence-')


def run(root, diagnostics):
    sidecar = diagnostics / 'qualification-evidence.json'
    candidate = check_candidate(root)
    run_id = secrets.token_hex(32)
    if not re.fullmatch(r'[0-9a-f]{64}', run_id):
        raise InputError('cannot generate a valid 256-bit run ID')
    info = json_data(command(['nix', 'store', 'info', '--store', 'daemon', '--json']))
    if not isinstance(info, dict) or info.get('url') != 'daemon' or info.get('trusted') is not True:
        raise InputError('qualification requires a trusted local Nix daemon')
    with tempfile.TemporaryDirectory(prefix='waddle-qualification-') as directory:
        archive = subprocess.Popen(['git', '-C', str(root), 'archive', '--format=tar', candidate],
                                   stdout=subprocess.PIPE, env=git_environment())
        try:
            subprocess.run(['tar', '-xf', '-', '-C', directory], stdin=archive.stdout, check=True)
        finally:
            if archive.stdout:
                archive.stdout.close()
        if archive.wait() != 0:
            raise InputError('cannot archive candidate')
        source = store_path(command(['nix', 'store', 'add-path', '--store', 'daemon', '--name',
                                     f'waddle-qualification-source-{candidate}-{run_id}', directory]).strip())
    verifier = git(root, 'show', f'{candidate}:server/scripts/verify_candidate_snapshot.py')
    try:
        subprocess.run([sys.executable, '-', candidate, str(source)], input=verifier, text=True, cwd=root, check=True)
    except subprocess.CalledProcessError as error:
        raise InputError('candidate source verification failed') from error
    gate = load_snapshot_gate(source)
    check_candidate(root, candidate)
    flake = f'path:{source}'
    isolation = ['--store', 'daemon', '--option', 'builders', '', '--option', 'sandbox', 'true',
                 '--option', 'sandbox-fallback', 'false']
    command(['nix', 'build', '--print-build-logs', '--no-link', *isolation,
             f'{flake}#waddle-ci-sandbox-probe'], cwd=root / 'server', capture=False)
    # Run the candidate's helper from the checkout, not from inside the source
    # store path: Nix first resolves an installable containing '/' as a path
    # relative to the working directory, so 'path:/nix/store/...#attr' under
    # /nix/store/<source>/server names the source store path itself.
    archive = store_path(command(['bash', str(source / 'server/scripts/build-nextest-archive.sh'),
                                  f'{flake}#waddle-server-test-archive'], cwd=root / 'server').strip())
    for name in ('ci-performance/cargo-timing.html', 'partition-coverage.json', 'plan.json'):
        atomic_copy(STORE_DIR, archive, Path(name), diagnostics / Path(name).name)
    wrapper = source / 'server/scripts/qualification-shards.nix'
    drvs = command(['nix-instantiate', '--store', 'daemon', str(wrapper), '--argstr', 'sourcePath', str(source),
                    '--argstr', 'candidateSha', candidate, '--argstr', 'runId', run_id]).splitlines()
    identities = derivation_identities(drvs, source, candidate, run_id)
    reject_existing_outputs(identities)
    results = command(['nix', 'build', '--print-build-logs', '--no-link', '--json', '--keep-going',
                       '--max-jobs', '4', '--cores', '8', *isolation, '--option', 'substitute', 'false',
                       *(f'{drv}^out' for drv in drvs)], cwd=root / 'server')
    validate_build_results(identities, results)
    atomic_write_text(diagnostics / 'shard-results.json', results)
    for item in identities:
        destination = diagnostics / f"shard-{item['partition']}"
        ensure_real_directory(destination, create=True)
        for name in ARTIFACTS:
            atomic_copy(STORE_DIR, Path(item['output_path']), Path(name), destination / name)
    record = build_record(candidate, run_id, source, identities,
                          source / 'server/.config/required-cluster-tests.txt', diagnostics, gate)
    check_candidate(root, candidate)
    atomic_write(sidecar, record)
    print(f"All {record['required_test_gate']['required_count']} required tests executed and passed")


@contextmanager
def qualification_guard(diagnostics):
    # The persistent file serializes runners and recovery; the kernel releases
    # the advisory lock on process death. Never unlink or replace this file.
    path = diagnostics / '.qualification-run.guard'
    try:
        ensure_regular_output(path)
    except InputError as error:
        raise UnsafeQualificationGuard(f'{path}: qualification guard must be a regular file') from error
    flags = os.O_CREAT | os.O_RDWR | getattr(os, 'O_NOFOLLOW', 0)
    try:
        descriptor = os.open(path, flags, 0o600)
    except OSError as error:
        if error.errno == errno.ELOOP:
            raise UnsafeQualificationGuard(f'{path}: qualification guard must be a regular file') from error
        raise
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise UnsafeQualificationGuard(f'{path}: qualification guard must be a regular file')
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise InputError('cannot acquire nextest qualification run lock: another process holds the guard') from error
        yield
    finally:
        os.close(descriptor)


def create_recovery_lock(diagnostics, sidecar, action):
    lock = diagnostics / '.qualification-run.lock'
    try:
        lock.mkdir()
    except FileExistsError as error:
        raise InputError(f'{sidecar}: not {action} because a qualification run lock already exists') from error


def unlink_sidecar_entry(sidecar):
    try:
        info = sidecar.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise InputError(f'{sidecar}: cannot inspect unsafe diagnostic sidecar') from error
    if not stat.S_ISLNK(info.st_mode) and not stat.S_ISREG(info.st_mode):
        raise InputError(f'{sidecar}: diagnostic sidecar must be a regular file')
    sidecar.unlink()


def invalidate_sidecar_after_unsafe_guard(diagnostics, sidecar):
    create_recovery_lock(diagnostics, sidecar, 'invalidated')
    unlink_sidecar_entry(sidecar)


def invalidate_symlinked_sidecar(diagnostics, sidecar):
    create_recovery_lock(diagnostics, sidecar, 'removed')
    unlink_sidecar_entry(sidecar)


def recover_stale_lock(diagnostics):
    lock = diagnostics / '.qualification-run.lock'
    try:
        lock_info = lock.lstat()
    except FileNotFoundError:
        raise InputError('no stale nextest qualification run lock exists')
    except OSError as error:
        raise InputError('cannot inspect stale nextest qualification run lock') from error
    if not stat.S_ISDIR(lock_info.st_mode):
        raise InputError('stale nextest qualification run lock must be a real directory')
    # Recovery is an explicit operator action after the old processes stop.
    # Invalidate even an apparently complete sidecar before releasing the lock.
    sidecar = diagnostics / 'qualification-evidence.json'
    ensure_regular_output(sidecar)
    sidecar.unlink(missing_ok=True)
    lock.rmdir()
    print('Removed stale qualification lock and invalidated any previous sidecar')


def main():
    if len(sys.argv) > 2 or (len(sys.argv) == 2 and sys.argv[1] != '--recover-stale-lock'):
        print('usage: run-nextest-shards.sh [--recover-stale-lock]', file=sys.stderr)
        return 2
    try:
        root = Path(command(['git', 'rev-parse', '--show-toplevel']).strip()).resolve()
        if Path.cwd().resolve() != root / 'server':
            raise InputError('run-nextest-shards.sh must run from the server directory')
        diagnostics = prepare_diagnostics(root)
        sidecar = diagnostics / 'qualification-evidence.json'
        ensure_safe_sidecar(sidecar)
        with qualification_guard(diagnostics):
            if len(sys.argv) == 2:
                recover_stale_lock(diagnostics)
                return 0
            lock = diagnostics / '.qualification-run.lock'
            try:
                lock.mkdir()
            except FileExistsError as error:
                raise InputError(f'cannot acquire nextest qualification run lock: {lock}') from error
            keep_lock = False
            completed = False
            previous = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM)}
            try:
                # If this unlink fails, retain the lock. A previous sidecar
                # must never become visible again after a failed run.
                ensure_regular_output(sidecar)
                sidecar.unlink(missing_ok=True)
                run(root, diagnostics)
                completed = True
            except InterruptedRun as error:
                keep_lock = True
                return 128 + error.signum
            finally:
                for sig, handler in previous.items():
                    signal.signal(sig, handler)
                if not keep_lock:
                    if not completed:
                        try:
                            ensure_regular_output(sidecar)
                            sidecar.unlink(missing_ok=True)
                        except OSError:
                            keep_lock = True
                            raise
                    if not keep_lock:
                        lock.rmdir()
        return 0
    except CommandError as error:
        print(error, file=sys.stderr)
        return error.returncode
    except UnsafeDiagnosticSidecar as error:
        try:
            invalidate_symlinked_sidecar(diagnostics, sidecar)
        except (InputError, OSError) as lock_error:
            print(f'{error}; {lock_error}', file=sys.stderr)
            return 1
        print(f'{error}; unsafe sidecar removed and recovery lock retained', file=sys.stderr)
        return 1
    except UnsafeQualificationGuard as error:
        try:
            invalidate_sidecar_after_unsafe_guard(diagnostics, sidecar)
        except (InputError, OSError) as lock_error:
            print(f'{error}; {lock_error}', file=sys.stderr)
            return 1
        print(f'{error}; previous evidence invalidated and recovery lock retained', file=sys.stderr)
        return 1
    except (InputError, OSError, subprocess.SubprocessError) as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
