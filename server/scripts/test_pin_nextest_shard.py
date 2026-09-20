import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("pin-nextest-shard.py").resolve()
SPEC = importlib.util.spec_from_file_location("pin_nextest_shard", SCRIPT)
affinity = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(affinity)


class ShardAffinityTests(unittest.TestCase):
    def test_sparse_allowed_cpu_ids_form_disjoint_eight_cpu_partitions(self):
        allowed = set(range(12, 28)) | set(range(48, 64))
        partitions = [affinity.shard_cpus(allowed, index, 4, 8) for index in range(1, 5)]
        self.assertTrue(all(len(cpus) == 8 for cpus in partitions))
        self.assertEqual(set.union(*partitions), allowed)
        self.assertEqual(sum(map(len, partitions)), len(set.union(*partitions)))
        self.assertEqual(partitions[2], set(range(48, 56)))

    def test_extra_host_cpus_do_not_expand_shard_concurrency(self):
        partitions = [affinity.shard_cpus(range(64), index, 4, 8) for index in range(1, 5)]
        self.assertEqual(set.union(*partitions), set(range(32)))

    def test_insufficient_cpus_fail_before_changing_parent(self):
        with patch.object(affinity.os, "getppid", return_value=50), \
             patch.object(affinity.os, "sched_getaffinity", return_value=set(range(31))), \
             patch.object(affinity.os, "sched_setaffinity") as set_affinity:
            with self.assertRaisesRegex(ValueError, "require at least 32 allowed CPUs.*sees 31"):
                affinity.pin_parent(1, 4, 8, 50)
            set_affinity.assert_not_called()

    def test_invalid_partition_and_capacity_fail(self):
        for partition, count, cores in [(0, 4, 8), (5, 4, 8), (1, 0, 8), (1, 4, 0)]:
            with self.subTest(partition=partition, count=count, cores=cores):
                with self.assertRaises(ValueError):
                    affinity.shard_cpus(range(32), partition, count, cores)

    def test_different_parent_affinity_fails_before_mutation(self):
        with patch.object(affinity.os, "getppid", return_value=50), \
             patch.object(affinity.os, "sched_getaffinity", side_effect=[set(range(32)), {1}]), \
             patch.object(affinity.os, "sched_setaffinity") as set_affinity:
            with self.assertRaisesRegex(ValueError, "different allowed CPUs"):
                affinity.pin_parent(1, 4, 8, 50)
            set_affinity.assert_not_called()

    def test_failed_kernel_affinity_verification_is_an_error(self):
        allowed = set(range(32))
        with patch.object(affinity.os, "getppid", return_value=50), \
             patch.object(affinity.os, "sched_getaffinity", return_value=allowed), \
             patch.object(affinity.os, "sched_setaffinity"):
            with self.assertRaisesRegex(ValueError, "did not retain"):
                affinity.pin_parent(2, 4, 8, 50)

    def test_intermediate_process_or_subshell_is_rejected_before_mutation(self):
        with patch.object(affinity.os, "getppid", return_value=51), \
             patch.object(affinity.os, "sched_setaffinity") as set_affinity:
            with self.assertRaisesRegex(ValueError, "invoke.*directly"):
                affinity.pin_parent(1, 4, 8, 50)
            set_affinity.assert_not_called()

    def test_pid_one_can_be_the_calling_shell_in_a_pid_namespace(self):
        with patch.object(affinity.os, "getppid", return_value=1), \
             patch.object(affinity.os, "sched_getaffinity", return_value={0}), \
             patch.object(affinity.os, "sched_setaffinity") as set_affinity, \
             patch("builtins.print"):
            affinity.pin_parent(1, 1, 1, 1)
            set_affinity.assert_called_once_with(1, {0})

    @unittest.skipUnless(hasattr(os, "sched_setaffinity"), "Linux CPU affinity is required")
    def test_real_parent_and_later_children_inherit_only_the_selected_cpu(self):
        if len(os.sched_getaffinity(0)) < 2:
            self.skipTest("two allowed CPUs are required for the native affinity probe")
        # Pin a separate process so this test runner keeps its original mask.
        # Its later child must inherit the narrowed mask without extra flags.
        program = """
import json, os, subprocess, sys
initial = sorted(os.sched_getaffinity(0))
subprocess.run([sys.executable, sys.argv[1], '--parent-pid', str(os.getpid()), '--partition', '2', '--count', '2', '--cores', '1'], check=True)
parent = sorted(os.sched_getaffinity(0))
child = json.loads(subprocess.check_output([sys.executable, '-c', 'import json, os; print(json.dumps(sorted(os.sched_getaffinity(0))))'], text=True))
print(json.dumps({'initial': initial, 'parent': parent, 'child': child}))
"""
        completed = subprocess.run(
            [sys.executable, "-c", program, str(SCRIPT)],
            text=True, capture_output=True, check=True,
        )
        lines = completed.stdout.splitlines()
        self.assertIn("phase=shard_affinity shard=2", lines[0])
        self.assertIn("assigned_cpu_count=1", lines[0])
        observed = json.loads(lines[-1])
        self.assertEqual(observed["parent"], [observed["initial"][1]])
        self.assertEqual(observed["child"], observed["parent"])


if __name__ == "__main__":
    unittest.main()
