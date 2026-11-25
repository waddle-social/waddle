"""Reconcile the merge_queue rule in Waddle's existing main ruleset."""

import hashlib
import json
import os
import subprocess
import sys

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
    if payload is not None:
        command += ["--method", "PUT", "--input", "-"]
    result = subprocess.run(command, input=json.dumps(payload) if payload else None,
                            text=True, stdout=subprocess.PIPE, check=True)
    return json.loads(result.stdout)


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
    if len(queues) != 1 or queues[0].get("parameters", {}).keys() != desired.keys():
        raise ValueError("Expected a single merge_queue rule with the declared parameter names")
    current = queues[0]["parameters"]
    body = {k: live[k] for k in WRITABLE}
    body["rules"] = [{**r, "parameters": desired} if r["type"] == "merge_queue" else r for r in rules]
    fingerprint = json.dumps([live, desired], sort_keys=True, separators=(",", ":"))
    return current, body, hashlib.sha256(fingerprint.encode()).hexdigest()


def reconcile(mode, desired, reviewed):
    if api(f"repos/{REPO}")["default_branch"] != "main":
        raise ValueError("Default branch has changed")
    current, body, fingerprint = plan(api(ENDPOINT), desired)
    if current == desired:
        print("The main merge queue matches the declared policy.")
        return 0
    for key, value in desired.items():
        if current[key] != value:
            print(f"{key}: {current[key]} -> {value}")
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
    if any(after[k] != body[k] for k in WRITABLE):
        raise ValueError("Ruleset readback differs from the reviewed update; inspect ruleset history")
    print("Applied and verified the main merge queue policy.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(reconcile(sys.argv[1], json.loads(sys.argv[2]), os.getenv("WADDLE_QUEUE_PLAN")))
    except (KeyError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
