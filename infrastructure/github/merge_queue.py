"""Reconcile Waddle’s main merge queue and its ordinary PR smoke requirement."""

from copy import deepcopy
import hashlib
import json
import os
import subprocess
import sys
from tempfile import NamedTemporaryFile

REPO = "waddle-social/waddle"
RULESET_ID = 6335231
ENDPOINT = f"repos/{REPO}/rulesets/{RULESET_ID}"
WRITABLE = {"name", "target", "conditions", "enforcement", "bypass_actors", "rules"}
READ_ONLY = {
    "id", "source", "source_type", "node_id", "created_at", "updated_at",
    "current_user_can_bypass", "_links",
}


def api(endpoint, payload=None):
    command = ["gh", "api", "--hostname", "github.com", "-H", "X-GitHub-Api-Version: 2022-11-28", endpoint]
    if payload is None:
        return json.loads(subprocess.check_output(command))
    with NamedTemporaryFile(mode="w", encoding="utf-8", suffix=".json") as body:
        json.dump(payload, body)
        body.flush()
        return json.loads(subprocess.check_output([*command, "--method", "PUT", "--input", body.name]))


def add_required_check(rules, smoke):
    matches = [rule for rule in rules if rule["type"] == "required_status_checks"]
    if len(matches) > 1:
        raise ValueError("Expected at most one required_status_checks rule")
    if not matches:
        rule = {"type": "required_status_checks", "parameters": {
            "strict_required_status_checks_policy": False, "required_status_checks": [],
        }}
        rules.append(rule)
    else:
        rule = matches[0]

    checks = rule["parameters"]["required_status_checks"]
    named = [check for check in checks if check["context"].casefold() == smoke["context"].casefold()]
    if named:
        if len(named) != 1 or named[0] != smoke:
            raise ValueError(f"Conflicting required status check context or issuer for {smoke['context']}")
        return False
    checks.append(smoke)
    return True


def comparable(writable):
    # GitHub returns rules, requirements and actors in server-defined order.
    value = deepcopy(writable)
    value["bypass_actors"].sort(key=lambda actor: json.dumps(actor, sort_keys=True))
    value["rules"].sort(key=lambda rule: rule["type"])
    for rule in value["rules"]:
        if rule["type"] != "required_status_checks":
            continue
        parameters = rule["parameters"]
        parameters.setdefault("do_not_enforce_on_create", False)
        parameters.setdefault("strict_required_status_checks_policy", False)
        for check in parameters["required_status_checks"]:
            check.setdefault("integration_id", None)
        parameters["required_status_checks"].sort(key=lambda check: json.dumps(check, sort_keys=True))
    return value


def plan(live, desired):
    if (live["id"], live["source_type"], live["source"], live["name"], live["target"], live["enforcement"]) != (
        RULESET_ID, "Repository", REPO, "main", "branch", "active",
    ) or live["conditions"] != {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}}:
        raise ValueError("Unexpected main ruleset identity, enforcement or branch scope")
    if not WRITABLE <= live.keys() <= WRITABLE | READ_ONLY or not isinstance(live["bypass_actors"], list):
        raise ValueError("Ruleset fields changed or bypass actors are hidden; use an administrator session")

    rules = live["rules"]
    if any(not {"type"} <= r.keys() <= {"type", "parameters"} for r in rules):
        raise ValueError("Unexpected fields in ruleset rules")
    queues = [r for r in rules if r["type"] == "merge_queue"]
    queue = desired["merge_queue"]
    smoke = desired["required_status_check"]
    if len(queues) != 1 or queues[0].get("parameters", {}).keys() != queue.keys():
        raise ValueError("Expected a single merge_queue rule with the declared parameter names")
    current = queues[0]["parameters"]
    body = deepcopy({k: live[k] for k in WRITABLE})
    next(r for r in body["rules"] if r["type"] == "merge_queue")["parameters"] = queue
    changes = [f"{key}: {current[key]} -> {value}" for key, value in queue.items() if current[key] != value]
    if add_required_check(body["rules"], smoke):
        changes.append(f"Require {smoke['context']} from GitHub App {smoke['integration_id']}")
    fingerprint = json.dumps([{**live, **comparable({k: live[k] for k in WRITABLE})}, desired],
                             sort_keys=True, separators=(",", ":"))
    return changes, body, hashlib.sha256(fingerprint.encode()).hexdigest()


def reconcile(mode, desired, reviewed):
    if api(f"repos/{REPO}")["default_branch"] != "main":
        raise ValueError("Default branch has changed")
    changes, body, fingerprint = plan(api(ENDPOINT), desired)
    if not changes:
        print("The main merge queue matches the declared policy.")
        return 0
    for change in changes:
        print(change)
    if mode == "check":
        return 1
    if mode == "plan":
        print(f"WADDLE_QUEUE_PLAN={fingerprint}")
        return 0
    if mode != "apply" or reviewed != fingerprint:
        raise ValueError("Review a fresh plan and supply its WADDLE_QUEUE_PLAN fingerprint")
    api(ENDPOINT, body)
    after = api(ENDPOINT)
    plan(after, desired)
    if comparable({k: after[k] for k in WRITABLE}) != comparable(body):
        raise ValueError("Ruleset readback differs from the reviewed update; inspect ruleset history")
    print("Applied and verified the main merge queue policy.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(reconcile(sys.argv[1], json.loads(sys.argv[2]), os.getenv("WADDLE_QUEUE_PLAN")))
    except (KeyError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
