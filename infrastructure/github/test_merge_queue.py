import unittest
from unittest.mock import patch

from merge_queue import REPO, RULESET_ID, plan, reconcile


class MergeQueueTest(unittest.TestCase):
    def setUp(self):
        self.desired = {"min_entries_to_merge_wait_minutes": 30}
        self.status_check = {"type": "required_status_checks", "parameters": {
            "required_status_checks": [{"context": "CI", "integration_id": 15368}],
            "strict_required_status_checks_policy": False,
        }}
        self.live = {
            "id": RULESET_ID, "name": "main", "source": REPO, "source_type": "Repository",
            "target": "branch", "enforcement": "active", "updated_at": "2026-11-25T00:00:00Z",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "bypass_actors": [{"actor_type": "User", "actor_id": 123, "bypass_mode": "always"}],
            "rules": [self.status_check, {"type": "merge_queue", "parameters": {
                "min_entries_to_merge_wait_minutes": 10,
            }}],
        }

    def test_only_changes_queue(self):
        _, body, _ = plan(self.live, self.desired)
        self.assertEqual(body, {
            "name": "main", "target": "branch", "conditions": self.live["conditions"],
            "enforcement": "active", "bypass_actors": self.live["bypass_actors"],
            "rules": [self.status_check, {"type": "merge_queue", "parameters": self.desired}],
        })

    def test_refuses_incomplete_or_unexpected_responses(self):
        queue = self.live["rules"][1]
        for change in [
            {"source_type": "Organization"}, {"conditions": {}}, {"id": RULESET_ID + 1},
            {"bypass_actors": None}, {"unknown_field": True}, {"rules": []},
            {"rules": [queue, queue]}, {"rules": [{**queue, "read_only_metadata": 1}]},
            {"rules": [{**queue, "parameters": {"unknown_parameter": True}}]},
        ]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                plan({**self.live, **change}, self.desired)
        del self.live["bypass_actors"]
        with self.assertRaises(ValueError):
            plan(self.live, self.desired)

    @patch("merge_queue.api")
    def test_stale_plan_after_concurrent_change_cannot_write(self, api):
        _, _, reviewed = plan(self.live, self.desired)
        api.side_effect = [{"default_branch": "main"}, {**self.live, "bypass_actors": []}]
        with self.assertRaisesRegex(ValueError, "fresh plan"):
            reconcile("apply", self.desired, reviewed)
        self.assertEqual(api.call_count, 2)

    @patch("merge_queue.api")
    def test_checks_fresh_readback_and_is_idempotent(self, api):
        _, expected, reviewed = plan(self.live, self.desired)
        applied = {**self.live, **expected}
        repo = {"default_branch": "main"}
        api.side_effect = [repo, self.live, {}, applied, repo, applied]
        self.assertEqual(reconcile("apply", self.desired, reviewed), 0)
        self.assertEqual(reconcile("apply", self.desired, reviewed), 0)
        writes = [c for c in api.call_args_list if len(c.args) > 1]
        self.assertEqual([c.args[1] for c in writes], [expected])

    @patch("merge_queue.api")
    def test_check_exits_nonzero_on_drift_without_writing(self, api):
        api.side_effect = [{"default_branch": "main"}, self.live]
        self.assertEqual(reconcile("check", self.desired, None), 1)
        self.assertEqual(api.call_count, 2)

    @patch("merge_queue.api")
    def test_reports_changed_sibling_on_readback(self, api):
        _, body, reviewed = plan(self.live, self.desired)
        api.side_effect = [{"default_branch": "main"}, self.live, {}, {**self.live, **body, "bypass_actors": []}]
        with self.assertRaisesRegex(ValueError, "readback differs"):
            reconcile("apply", self.desired, reviewed)


if __name__ == "__main__":
    unittest.main()
