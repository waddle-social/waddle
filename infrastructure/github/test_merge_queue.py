from copy import deepcopy
import unittest
from unittest.mock import patch

from merge_queue import REPO, RULESET_ID, plan, reconcile


class MergeQueueTest(unittest.TestCase):
    def setUp(self):
        self.smoke = {"context": "PR smoke", "integration_id": 15368}
        self.desired = {"merge_queue": {"min_entries_to_merge_wait_minutes": 30}, "required_status_check": self.smoke}
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

    def test_preserves_other_requirements_and_rules(self):
        _, body, _ = plan(self.live, self.desired)
        self.assertEqual(body, {
            "name": "main", "target": "branch", "conditions": self.live["conditions"],
            "enforcement": "active", "bypass_actors": self.live["bypass_actors"],
            "rules": [
                {"type": "required_status_checks", "parameters": {
                    **self.status_check["parameters"],
                    "required_status_checks": [self.status_check["parameters"]["required_status_checks"][0], self.smoke],
                }},
                {"type": "merge_queue", "parameters": self.desired["merge_queue"]},
            ],
        })

    def test_creates_non_strict_rule_and_preserves_existing_strict_policy(self):
        _, body, _ = plan({**self.live, "rules": [self.live["rules"][1]]}, self.desired)
        self.assertEqual(body["rules"][-1], {"type": "required_status_checks", "parameters": {
            "required_status_checks": [self.smoke], "strict_required_status_checks_policy": False,
        }})
        self.status_check["parameters"]["strict_required_status_checks_policy"] = True
        _, existing, _ = plan(self.live, self.desired)
        self.assertTrue(existing["rules"][0]["parameters"]["strict_required_status_checks_policy"])

    def test_refuses_conflicting_issuers_and_duplicate_status_rules(self):
        for other in [None, 42]:
            with self.subTest(issuer=other), self.assertRaisesRegex(ValueError, "Conflicting"):
                self.status_check["parameters"]["required_status_checks"] = [{**self.smoke, "integration_id": other}]
                plan(self.live, self.desired)
        with self.assertRaisesRegex(ValueError, "at most one"):
            plan({**self.live, "rules": [*self.live["rules"], self.status_check]}, self.desired)

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
        applied = deepcopy({**self.live, **expected})
        applied["rules"].reverse()
        status = applied["rules"][-1]["parameters"]
        status["required_status_checks"].reverse()
        status["do_not_enforce_on_create"] = False
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
