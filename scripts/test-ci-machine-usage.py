#!/usr/bin/env python3
"""Contract tests for scripts/ci-machine-usage.py."""

import importlib.util
import io
import json
import os
from contextlib import redirect_stdout
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('ci-machine-usage.py')
spec = importlib.util.spec_from_file_location('ci_machine_usage', SCRIPT)
usage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usage)

STAT = 'cpu  {user} 0 0 {idle} 0 0 0 0 0 0\ncpu0 1 0 0 1 0 0 0 0 0 0\ncpu1 1 0 0 1 0 0 0 0 0 0\nintr 1\n'
MEMINFO = 'MemTotal:       33554432 kB\nMemFree:  1 kB\nMemAvailable:   {available} kB\n'


def samples(rows):
    """rows: (time, cumulative busy ticks, cumulative idle ticks, available kB)."""
    return [usage.read_sample(STAT.format(user=busy, idle=idle), MEMINFO.format(available=available), when)
            for when, busy, idle, available in rows]


class MachineUsage(unittest.TestCase):
    def test_sample_counts_cpus_and_used_memory(self):
        record = samples([(10.0, 300, 100, 25165824)])[0]
        self.assertEqual((record['cpus'], record['busy'], record['total']), (2, 300, 400))
        self.assertEqual(record['mem_used_kib'], 8388608)

    def test_usage_reports_busy_cores_and_peak_memory_in_a_window(self):
        data = samples([(0, 0, 0, 33554432), (2, 100, 100, 16777216), (4, 300, 100, 25165824),
                        (6, 300, 300, 33554432)])
        whole = usage.usage(data)
        self.assertEqual(whole['max_busy_cores'], 2.0)  # both CPUs busy in the second interval
        self.assertEqual(whole['mean_busy_cores'], 1.0)
        self.assertEqual(whole['peak_memory_gib'], 16.0)
        window = usage.usage(data, start=2, end=4)
        self.assertEqual((window['mean_busy_cores'], window['peak_memory_gib'], window['seconds']), (2.0, 16.0, 2.0))
        self.assertIsNone(usage.usage(data, start=5, end=5.5))

    def test_report_and_summary_use_phase_windows_from_the_timestamped_log(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            os.environ['RUNNER_TEMP'] = directory
            (root / 'samples.jsonl').write_text(''.join(json.dumps(item) + '\n' for item in samples(
                [(0, 0, 0, 33554432), (10, 1000, 1000, 16777216), (20, 3000, 1000, 25165824),
                 (30, 3200, 2800, 33554432), (40, 3400, 4600, 33554432)])))
            (root / 'lane.log').write_text(
                '0.000 archive> WADDLE_CI_METRIC phase=compile linker=mold version=2.42.0\n'
                'not-a-time WADDLE_CI_METRIC phase=compile elapsed_seconds=1\n'
                '20.000 archive> WADDLE_CI_METRIC phase=compile elapsed_seconds=20.5 cpu=311%\n'
                '20.500 shard> WADDLE_CI_METRIC phase=shard_affinity shard=1 assigned_cpu_count=4\n'
                '30.000 shard> WADDLE_CI_METRIC phase=tests shard=1 selection=whole elapsed_seconds=9.5\n'
                '40.000 shard> WADDLE_CI_METRIC phase=tests shard=1 selection=shared elapsed_seconds=10\n'
                '40.000 shard> WADDLE_CI_METRIC phase=tests shard=2 selection=whole elapsed_seconds=12\n')
            (root / 'out').mkdir()
            result = subprocess.run(
                [sys.executable, str(SCRIPT), 'report', '--samples', str(root / 'samples.jsonl'),
                 '--log', str(root / 'lane.log'), '--variant', '16x32', '--vcpu', '16', '--memory-gib', '32',
                 '--outcome', 'success', '--lane-seconds', '600'],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            (root / 'out/16x32.json').write_text(result.stdout)
            record = json.loads(result.stdout)
            self.assertEqual(record['compile_seconds'], 20.5)
            self.assertEqual(record['compile']['max_busy_cores'], 2.0)
            self.assertEqual(record['shard_seconds'], {'1': 19.5, '2': 12.0})
            self.assertEqual(record['tests']['mean_busy_cores'], 0.2)
            self.assertEqual((record['cpus_seen'], record['memory_total_gib']), (2, 32.0))
            (root / 'out/failed.json').write_text(json.dumps(
                {'variant': '8x32', 'vcpu': 8, 'memory_gib': 32, 'outcome': 'failure', 'lane_seconds': None,
                 'machine': None, 'compile_seconds': None, 'compile': None, 'shard_seconds': {}, 'tests': None}))
            output = io.StringIO()
            with redirect_stdout(output):
                usage.summarize(root / 'out')
            table = output.getvalue()
            # 16 vCPU x 10 min = 160 vCPU-min; four 16x32 jobs fit under 64 vCPU / 128 GB.
            self.assertIn('| 16x32 | 16x32 | success | 10.0 | 0.3 | 0.3 | 160 | 4 | 1.5 / 2.0 | 0.2 / 0.2 | 16.0 |', table)
            self.assertIn('| 8x32 | 8x32 | failure | — | — | — | — | 4 | — | — | — |', table)
            self.assertLess(table.index('| 16x32 |'), table.index('| 8x32 |'))

    def test_file_arguments_outside_the_workspace_and_runner_temp_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            os.environ['RUNNER_TEMP'] = directory
            self.assertEqual(usage.confined(Path(directory) / 'a.json'), (Path(directory) / 'a.json').resolve())
            self.assertEqual(usage.confined('scripts/x'), Path('scripts/x').resolve())
            with self.assertRaises(SystemExit):
                usage.confined(Path(directory) / '../escape.json')


if __name__ == '__main__':
    unittest.main()
