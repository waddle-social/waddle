import { appendFileSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import taskPaths from "../ci/rust-tests/queue-paths.json";

const workflowDirectory = ".github/workflows";
export const outputFile = `${workflowDirectory}/waddle-integration.yml`;
const checkout = "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5";
const setupBun = "oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6";
// Explicitly reviewed source aliases; see docs/integration-ci.md for provenance/runtime requirements.
const reviewedActionPins: Record<string, string> = {
  "actions/checkout@v7": "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
  "actions/checkout@v4": "actions/checkout@11d5960a326750d5838078e36cf38b85af677262",
  "actions/setup-python@v5": "actions/setup-python@a26af69be951a213d495a4c3e4e4022e16d87065",
};
const immutableAction = /^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*(?:\/[A-Za-z0-9_][A-Za-z0-9_.-]*)*@[0-9a-fA-F]{40}$/;

function pinActionReferences(value: any): any {
  if (Array.isArray(value)) return value.map(pinActionReferences);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(Object.entries(value).map(([key, item]) => {
    if (key !== "uses") return [key, pinActionReferences(item)];
    const action = typeof item === "string" ? (reviewedActionPins[item] ?? item) : item;
    if (typeof action !== "string" || !immutableAction.test(action)) throw new Error(`Unreviewed mutable action reference: ${String(item)}`);
    return [key, action];
  }));
}

type Workflow = Record<string, any>;
export type Lane = { id: string; file: string; workflow: Workflow; paths?: string[]; mainOwner?: { file: string; paths: Bun.Glob[] } };

// These verification workflows run on demand or after merge. The queue reuses
// their jobs and the path filters of the matching project workflows. Only
// trigger paths are read from default workflows; publishing jobs stay on main.
const queueScopes: Record<string, string[]> = {
  "operational-qualification": ["operational-qualification"],
  "waddle-server-xmppcompliance": ["waddle-server-xmppcompliance"],
  "waddle-server-authzidcodec": ["waddle-server-authzidcodec"],
  "waddle-server-rootsync": ["waddle-server-rootsync"],
  "waddle-apple-default": ["waddle-apple-default"],
  "waddle-server-federationgrantfence": ["waddle-server-federationgrantfence"],
  ...Object.fromEntries(["chat", "operator"].map(project => [
    `waddle-${project}-queuechecks`, [`waddle-${project}-queuechecks`, `waddle-${project}-default`],
  ])),
  ...Object.fromEntries(["android", "linux"].map(project => [
    `waddle-${project}-queuechecks`, [`waddle-${project}-pullrequest`, `waddle-${project}-default`],
  ])),
};

// The standalone Apple workflow and the server publisher each run the same
// verification command as the corresponding candidate lane.
const mainJobs: Record<string, { file: string; job: string }> = {
  "waddle-apple-default": { file: "waddle-apple-default.yml", job: "apple-xmpp-client" },
  "waddle-server-namespacerustchecks": { file: "waddle-server-default.yml", job: "mainTest" },
};

function triggerPaths(trigger: any, file: string): string[] | undefined {
  if (trigger?.["paths-ignore"]) throw new Error(`Unsupported paths-ignore in ${file}`);
  const paths = trigger?.paths;
  if (paths !== undefined && (!Array.isArray(paths) || paths.some(path => typeof path !== "string"))) {
    throw new Error(`Invalid paths in ${file}`);
  }
  return paths;
}

export function readLanes(directory = workflowDirectory): Lane[] {
  const workflows = new Map<string, Workflow>();
  for (const file of readdirSync(directory).filter(file => /\.ya?ml$/.test(file)).sort()) {
    if (file === "waddle-integration.yml" || file === "waddle-pr-smoke.yml") continue;
    const workflow = Bun.YAML.parse(readFileSync(join(directory, file), "utf8")) as Workflow;
    if (typeof workflow.on === "string") workflow.on = { [workflow.on]: {} };
    if (Array.isArray(workflow.on)) workflow.on = Object.fromEntries(workflow.on.map((event: string) => [event, {}]));
    workflows.set(file, workflow);
  }

  const deferred = new Map<string, string[] | undefined>(Object.entries(taskPaths));
  for (const [name, scopes] of Object.entries(queueScopes)) {
    const filters = scopes.map(scope => {
      if (deferred.has(scope)) return deferred.get(scope);
      const source = workflows.get(scope + ".yml");
      if (!source?.on || !["pull_request", "push"].some(event => Object.hasOwn(source.on, event))) {
        throw new Error(`Missing path source ${scope} for ${name}`);
      }
      return triggerPaths(source.on.pull_request ?? source.on.push, scope);
    });
    deferred.set(name, filters.every(filter => filter !== undefined) ? [...new Set(filters.flat())] : undefined);
  }
  for (const name of deferred.keys()) {
    if (!workflows.has(name + ".yml")) throw new Error(`Missing deferred workflow ${name}`);
  }

  return [...workflows].flatMap(([file, workflow]) => {
    const name = file.replace(/\.ya?ml$/, "");
    let paths: string[] | undefined;
    if (deferred.has(name)) {
      if (!workflow.on || !Object.hasOwn(workflow.on, "workflow_dispatch") || Object.hasOwn(workflow.on, "pull_request") || Object.hasOwn(workflow.on, "merge_group")) {
        throw new Error(`Expected separately dispatched queue verification in ${file}`);
      }
      const scope = deferred.get(name);
      if (scope) paths = [`.github/workflows/${file}`, ...scope];
    } else if (workflow.on && Object.hasOwn(workflow.on, "pull_request")) {
      paths = triggerPaths(workflow.on.pull_request, file);
    } else {
      return [];
    }
    const lane: Lane = { id: name.replaceAll("-", "_"), file, workflow, paths };
    const owner = mainJobs[name];
    if (owner) {
      const main = workflows.get(owner.file);
      const push = main?.on?.push;
      const filters = push && !push["paths-ignore"] && triggerPaths(push, owner.file);
      if (main?.jobs[owner.job] && !main.jobs[owner.job].if && push?.branches?.length === 1 && push.branches[0] === "main" &&
          filters?.length && filters.every((pattern: string) => simpleGlob.test(pattern))) {
        lane.mainOwner = { file: owner.file, paths: filters.map((pattern: string) => new Bun.Glob(pattern)) };
      }
    }
    return [lane];
  });
}

const graphInputs = new Set([
  outputFile, "ci/rust-tests/queue-paths.cue", "ci/rust-tests/queue-paths.json", "scripts/sync-rust-tests.sh",
  "scripts/integration-ci.ts", "scripts/integration-ci.test.ts", "scripts/integration-ci-project.py",
  "scripts/test_integration_ci_project.py", "scripts/test_cuenv_bootstrap.py",
]);
const simpleGlob = /^(?:(?:[A-Za-z0-9_.-]+|\*\*)\/)*(?:[A-Za-z0-9_.-]+|\*\*|\*\.cue)$/;

export function applicable(lane: Lane, files: string[]): boolean {
  // A graph or planner change exercises every lane. Source PR branch filters
  // are deliberately ignored: queue validation can conservatively add coverage.
  if (files.some(file => graphInputs.has(file))) return true;
  const patterns = lane.paths ?? lane.workflow.on.pull_request?.paths;
  if (!patterns) return true;
  if (!Array.isArray(patterns) || patterns.some(pattern => typeof pattern !== "string")) throw new Error(`Invalid paths in ${lane.file}`);
  // GitHub accepts more glob syntax than Bun. Unknown syntax selects the lane
  // rather than risking a false negative. Checked-in patterns currently use *.
  if (patterns.some(pattern => /[!+\[\]{}()\\]/.test(pattern))) return true;
  return files.some(file => patterns.some(pattern => new Bun.Glob(pattern).match(file) || (!pattern.includes("*") && file.startsWith(pattern + "/"))));
}

export function selectLanes(lanes: Lane[], files: string[], mergingToMain: boolean): Lane[] {
  // Classify unknown surfaces before moving work to main. Apple-only changes
  // still belong to the Apple job even when main will execute that job.
  const unknown = files.some(file => !lanes.some(lane => applicable(lane, [file])));
  const selected = unknown ? lanes : lanes.filter(lane => applicable(lane, files));
  if (!mergingToMain || files.length > 300 || files.some(file => graphInputs.has(file))) return selected;
  return selected.filter(lane => {
    const owner = lane.mainOwner;
    if (!owner || !lane.paths?.every(pattern => simpleGlob.test(pattern))) return true;
    if (files.includes(`.github/workflows/${lane.file}`) || files.includes(`.github/workflows/${owner.file}`)) return true;
    if (owner.file === "waddle-server-default.yml" && files.some(file => file === "server/env.cue" || file === "namespace.cue")) return true;

    // Every changed input for this lane must also trigger the owning workflow.
    // Fixture-only and mixed fixture/source changes retain the candidate check.
    const covered = (file: string) => owner.paths.some(glob => glob.match(file));
    return !files.some(covered) || files.some(file => applicable(lane, [file]) && !covered(file));
  });
}

export function jobIds(lane: Lane): string[] {
  return Object.keys(lane.workflow.jobs).map(id => `${lane.id}__${id.toLowerCase()}`);
}

function remapNeeds(value: any, lane: Lane): any {
  if (typeof value === "string") {
    for (const id of Object.keys(lane.workflow.jobs).sort((a, b) => b.length - a.length)) {
      value = value.replaceAll(`needs['${id}']`, `needs['${lane.id}__${id.toLowerCase()}']`)
        .replaceAll(`needs["${id}"]`, `needs["${lane.id}__${id.toLowerCase()}"]`)
        .replace(new RegExp(`needs\\.${id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\w-])`, "g"), `needs.${lane.id}__${id.toLowerCase()}`);
    }
    return value;
  }
  if (Array.isArray(value)) return value.map(item => remapNeeds(item, lane));
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, remapNeeds(item, lane)]));
  return value;
}

const utilitySteps = () => [
  { name: "Checkout exact candidate", uses: checkout, with: { ref: "${{ github.sha }}", "fetch-depth": 0, "persist-credentials": false } },
  { name: "Install Bun", uses: setupBun, with: { "bun-version": "1.4.4" } },
];

export function generate(lanes: Lane[]): string {
  if (!lanes.length) throw new Error("No PR verification lanes discovered");
  const inventory = lanes.flatMap(jobIds);
  if (new Set(inventory).size !== inventory.length) throw new Error("Duplicate integration job IDs");
  const jobs: Workflow = {
    plan: {
      name: "Plan exact candidate verification", "runs-on": "ubuntu-24.04", "timeout-minutes": 5,
      permissions: { contents: "read" },
      outputs: Object.fromEntries(["expected", "base", ...lanes.map(lane => lane.id)].map(key => [key, `\${{ steps.plan.outputs.${key} }}`])),
      steps: [...utilitySteps(),
        { name: "Check graph parity and planner contracts", run: "bun scripts/integration-ci.ts --check\nbun test scripts/integration-ci.test.ts\npython3 scripts/test_integration_ci_project.py\npython3 scripts/test_cuenv_bootstrap.py" },
        { id: "plan", name: "Select every applicable lane", run: "bun scripts/integration-ci.ts plan" },
      ],
    },
  };
  for (const lane of lanes) {
    if (Object.keys(lane.workflow).some(key => !["name", "on", "concurrency", "permissions", "jobs", "env", "defaults", "run-name"].includes(key))) throw new Error(`Unsupported workflow setting in ${lane.file}`);
    for (const [id, source] of Object.entries(lane.workflow.jobs) as [string, Workflow][]) {
      if (source.uses || source.strategy || source.concurrency) throw new Error(`Unsupported job shape in ${lane.file}/${id}`);
      const job = remapNeeds(structuredClone(source), lane);
      const dependencies = source.needs ? (Array.isArray(source.needs) ? source.needs : [source.needs]) : [];
      job.needs = ["plan", ...dependencies.map((dependency: string) => `${lane.id}__${dependency.toLowerCase()}`)];
      const condition = source.if ? String(source.if).replace(/^\$\{\{\s*|\s*\}\}$/g, "") : "success()";
      job.if = `\${{ (${condition}) && needs.plan.result == 'success' && needs.plan.outputs.${lane.id} == 'true' }}`;
      job.name = `${lane.workflow.name} / ${source.name ?? id}`;
      // Verification code must not inherit reporter writes or cloud identity.
      // FlakeHub cache requires OIDC, so keep Hestia and omit that setup step.
      job.steps = job.steps.filter((step: Workflow) => !step.uses?.startsWith("DeterminateSystems/flakehub-cache-action@"));
      if (lane.workflow.env) job.env = { ...lane.workflow.env, ...job.env };
      if (lane.workflow.defaults) job.defaults = { ...lane.workflow.defaults, ...job.defaults };
      let checkouts = 0;
      for (const step of job.steps ?? []) {
        if (step.run?.includes("cuenv ci")) {
          const invocation = step.run.match(/^cuenv ci --pipeline ([a-zA-Z0-9_-]+) --path ([a-zA-Z0-9_./-]+)$/);
          if (!invocation || step["working-directory"]) throw new Error(`Unsupported pipeline invocation in ${lane.file}/${id}`);
          // Original CI execution preserves runtime/dependency behavior.
          // Fail closed until a supported --all CLI is explicitly pinned.
          step.run = [
            "set -euo pipefail",
            'test "$(git rev-parse HEAD)" = "$WADDLE_CI_HEAD_SHA"',
            'git cat-file -e "${WADDLE_CI_BASE_SHA}^{commit}"',
            'git merge-base --is-ancestor "$WADDLE_CI_BASE_SHA" "$WADDLE_CI_HEAD_SHA"',
            'git -c diff.renames=false diff --no-renames --name-only -z "$WADDLE_CI_BASE_SHA...$WADDLE_CI_HEAD_SHA" > /dev/null',
            "WADDLE_TEST_ALL_CUENV=1 python3 scripts/test_integration_ci_project.py",
            `python3 scripts/integration-ci-project.py --path ${invocation[2]} --pipeline ${invocation[1]} --run`,
          ].join("\n");
          step.env = { ...step.env, WADDLE_CI_BASE_SHA: "${{ needs.plan.outputs.base }}", WADDLE_CI_HEAD_SHA: "${{ github.sha }}" };
        }
        if (step.uses?.startsWith("actions/checkout@")) {
          checkouts++;
          if (step.with?.repository || (step.with?.ref && step.with.ref !== "${{ github.sha }}")) throw new Error(`Unexpected checkout override in ${lane.file}/${id}`);
          step.with = { ...step.with, ref: "${{ github.sha }}", "persist-credentials": false, "fetch-depth": 0 };
        }
        if (/^actions\/(upload|download)-artifact@/.test(step.uses ?? "")) {
          if (!step.with?.name || step.with.pattern) throw new Error(`Unsupported artifact in ${lane.file}/${id}`);
          step.with.name = `${lane.id}-${step.with.name}`;
        }
      }
      job.permissions = checkouts ? { contents: "read" } : {};
      for (const environment of [job.env, ...job.steps.map((step: Workflow) => step.env)]) {
        if (environment) { delete environment.GITHUB_TOKEN; delete environment.GH_TOKEN; }
      }
      for (const owner of [job, ...job.steps]) {
        if (owner.env && !Object.keys(owner.env).length) delete owner.env;
      }
      if (/\$\{\{[^}]*secrets\./.test(JSON.stringify(job))) throw new Error(`Unreviewed secret use in ${lane.file}/${id}`);
      // The old nixTest aggregate has no checkout or source-executing steps.
      if (!checkouts && !dependencies.length) throw new Error(`Unbound source execution in ${lane.file}/${id}`);
      jobs[`${lane.id}__${id.toLowerCase()}`] = job;
    }
  }
  jobs.integration = {
    name: "integration", "runs-on": "ubuntu-24.04", "timeout-minutes": 5,
    if: "${{ always() }}", needs: Object.keys(jobs), permissions: { contents: "read" },
    steps: [...utilitySteps(), {
      name: "Require successful applicable jobs", run: "bun scripts/integration-ci.ts gate",
      env: { JOB_RESULTS: "${{ toJSON(needs) }}", EXPECTED_JOBS: "${{ needs.plan.outputs.expected }}" },
    }],
  };
  // Ordinary PR workflows provide fast feedback; the complete graph runs once
  // on the combined merge candidate, with an explicit dispatch for qualification.
  const workflow = {
    name: "waddle-integration",
    on: { merge_group: { types: ["checks_requested"] }, workflow_dispatch: {
      inputs: { base_sha: { description: "Full ancestor SHA to compare with the selected workflow ref", required: true, type: "string" } },
    } },
    permissions: { contents: "read" },
    concurrency: { group: "integration-${{ github.event_name }}-${{ github.head_ref || github.ref }}", "cancel-in-progress": true },
    jobs,
  };
  return "# Generated from PR and deferred verification workflows; do not edit manually.\n# Regenerate with: bun scripts/integration-ci.ts sync\n\n" + Bun.YAML.stringify(pinActionReferences(workflow), null, 2).replace(/[ \t]+$/gm, "") + "\n";
}

export function verifyGate(results: Record<string, { result: string }>, expected: string[], allJobs: string[]): void {
  if (results.plan?.result !== "success") throw new Error("Candidate planning did not succeed");
  if (!Array.isArray(expected) || new Set(expected).size !== expected.length || expected.some(id => !allJobs.includes(id))) throw new Error("Invalid expected job inventory");
  for (const id of allJobs) {
    const required = expected.includes(id);
    if (results[id]?.result !== (required ? "success" : "skipped")) throw new Error(`${id}: expected ${required ? "success" : "skipped"}, received ${results[id]?.result ?? "missing"}`);
  }
}

function git(args: string[]): string {
  const command = Bun.spawnSync(["git", ...args]);
  if (command.exitCode !== 0) throw new Error(`git ${args[0]} failed: ${command.stderr.toString()}`);
  return command.stdout.toString();
}

export function candidateBase(eventName: string, event: any, sha: string): string {
  if (!/^[a-f0-9]{40}$/.test(sha)) throw new Error("Invalid candidate SHA");
  let base: string;
  if (eventName === "merge_group") {
    if (event.action !== "checks_requested" || event.merge_group?.head_sha !== sha) throw new Error("Merge-group event does not match candidate");
    base = event.merge_group.base_sha;
  } else if (eventName === "pull_request") {
    if (!event.pull_request?.head?.sha || !event.pull_request?.base?.sha) throw new Error("Missing PR candidate metadata");
    base = event.pull_request.base.sha;
  } else if (eventName === "workflow_dispatch") base = event.inputs?.base_sha;
  else throw new Error(`Unsupported event: ${eventName}`);
  if (!/^[a-f0-9]{40}$/.test(base ?? "")) throw new Error("Invalid base SHA");
  return base;
}

if (import.meta.main) {
  const mode = process.argv[2];
  const lanes = readLanes();
  if (mode === "sync") writeFileSync(outputFile, generate(lanes));
  else if (mode === "--check") {
    if (readFileSync(outputFile, "utf8") !== generate(lanes)) throw new Error("Integration workflow drift: run bun scripts/integration-ci.ts sync");
  } else if (mode === "plan") {
    const sha = process.env.GITHUB_SHA ?? "";
    const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH!, "utf8"));
    const base = candidateBase(process.env.GITHUB_EVENT_NAME!, event, sha);
    if (git(["rev-parse", "HEAD"]).trim() !== sha) throw new Error("Checkout differs from candidate SHA");
    git(["merge-base", "--is-ancestor", base, sha]);
    const files = git(["diff", "--no-renames", "--name-only", "-z", base, sha]).split("\0").filter(Boolean);
    if (!files.length) throw new Error("Candidate diff is empty; refusing unverified no-op success");
    const mergingToMain = process.env.GITHUB_EVENT_NAME === "merge_group" && event.merge_group?.base_ref === "refs/heads/main";
    const selected = selectLanes(lanes, files, mergingToMain);
    appendFileSync(process.env.GITHUB_OUTPUT!, `base=${base}\n` + selected.map(lane => `${lane.id}=true\n`).join("") + `expected=${JSON.stringify(selected.flatMap(jobIds))}\n`);
    console.log(JSON.stringify({ candidate: sha, base, changedFiles: files, workflows: selected.map(lane => lane.file) }, null, 2));
  } else if (mode === "gate") {
    verifyGate(JSON.parse(process.env.JOB_RESULTS!), JSON.parse(process.env.EXPECTED_JOBS!), lanes.flatMap(jobIds));
    console.log("Every applicable verification job passed at this candidate SHA.");
  } else throw new Error("Usage: bun scripts/integration-ci.ts {sync|--check|plan|gate}");
}
