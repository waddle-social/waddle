import { appendFileSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const workflowDirectory = ".github/workflows";
export const outputFile = `${workflowDirectory}/waddle-integration.yml`;
const checkout = "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5";
const setupBun = "oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6";
type Workflow = Record<string, any>;
export type Lane = { id: string; file: string; workflow: Workflow };

export function readLanes(directory = workflowDirectory): Lane[] {
  return readdirSync(directory).filter(file => /\.ya?ml$/.test(file)).sort().flatMap(file => {
    if (file === "waddle-integration.yml") return [];
    const workflow = Bun.YAML.parse(readFileSync(join(directory, file), "utf8")) as Workflow;
    if (typeof workflow.on === "string") workflow.on = { [workflow.on]: {} };
    if (Array.isArray(workflow.on)) workflow.on = Object.fromEntries(workflow.on.map((event: string) => [event, {}]));
    if (!workflow.on || !Object.hasOwn(workflow.on, "pull_request")) return [];
    if (workflow.on.pull_request?.["paths-ignore"]) throw new Error(`Unsupported paths-ignore in ${file}`);
    const id = file.replace(/\.ya?ml$/, "").replaceAll("-", "_");
    return [{ id, file, workflow }];
  });
}

export function applicable(lane: Lane, files: string[]): boolean {
  // A graph or planner change exercises every lane. Source PR branch filters
  // are deliberately ignored: queue validation can conservatively add coverage.
  if (files.some(file => [outputFile, "scripts/integration-ci.ts", "scripts/integration-ci.test.ts", "scripts/integration-ci-project.py", "scripts/test_integration_ci_project.py"].includes(file))) return true;
  const patterns = lane.workflow.on.pull_request?.paths;
  if (!patterns) return true;
  if (!Array.isArray(patterns) || patterns.some(pattern => typeof pattern !== "string")) throw new Error(`Invalid paths in ${lane.file}`);
  // GitHub accepts more glob syntax than Bun. Unknown syntax selects the lane
  // rather than risking a false negative. Checked-in patterns currently use *.
  if (patterns.some(pattern => /[!+\[\]{}()\\]/.test(pattern))) return true;
  return files.some(file => patterns.some(pattern => new Bun.Glob(pattern).match(file)));
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
  { name: "Install Bun", uses: setupBun, with: { "bun-version": "1.4.2" } },
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
        { name: "Check graph parity and planner contracts", run: "bun scripts/integration-ci.ts --check\nbun test scripts/integration-ci.test.ts\npython3 scripts/test_integration_ci_project.py" },
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
          // cuenv 0.57 ignores --from under GitHub auto-detection and its
          // merge-group fallback only sees HEAD^; force the local provider.
          step.run = [
            "set -euo pipefail",
            'test "$(git rev-parse HEAD)" = "$WADDLE_CI_HEAD_SHA"',
            'git cat-file -e "${WADDLE_CI_BASE_SHA}^{commit}"',
            'git merge-base --is-ancestor "$WADDLE_CI_BASE_SHA" "$WADDLE_CI_HEAD_SHA"',
            'git -c diff.renames=false diff --no-renames --name-only -z "$WADDLE_CI_BASE_SHA...$WADDLE_CI_HEAD_SHA" > /dev/null',
            "WADDLE_TEST_PINNED_CUENV=1 python3 scripts/test_integration_ci_project.py",
            `python3 scripts/integration-ci-project.py --path ${invocation[2]} --pipeline ${invocation[1]}`,
            `env -u GITHUB_ACTIONS -u BUILDKITE GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=diff.renames GIT_CONFIG_VALUE_0=false ${step.run} --from "$WADDLE_CI_BASE_SHA"`,
          ].join("\n");
          step.env = { ...step.env, WADDLE_CI_BASE_SHA: "${{ needs.plan.outputs.base }}", WADDLE_CI_HEAD_SHA: "${{ github.sha }}" };
        }
        if (step.uses?.startsWith("actions/checkout@")) {
          checkouts++;
          if (step.with?.repository || step.with?.ref) throw new Error(`Unexpected checkout override in ${lane.file}/${id}`);
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
  const workflow = {
    name: "waddle-integration",
    on: { pull_request: {}, merge_group: { types: ["checks_requested"] }, workflow_dispatch: {
      inputs: { base_sha: { description: "Full ancestor SHA to compare with the selected workflow ref", required: true, type: "string" } },
    } },
    permissions: { contents: "read" },
    concurrency: { group: "integration-${{ github.event_name }}-${{ github.head_ref || github.ref }}", "cancel-in-progress": true },
    jobs,
  };
  return "# Generated from existing PR workflow graphs; do not edit manually.\n# Regenerate with: bun scripts/integration-ci.ts sync\n\n" + Bun.YAML.stringify(workflow, null, 2).replace(/[ \t]+$/gm, "") + "\n";
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
    const matches = lanes.filter(lane => applicable(lane, files));
    // An unrecognized surface must not silently become a zero-job success.
    const selected = files.some(file => !lanes.some(lane => applicable(lane, [file]))) ? lanes : matches;
    // cuenv 0.57 parses newline-delimited git output internally. Refuse any
    // encoded path that its affected-task parser could silently misinterpret.
    if (selected.some(lane => JSON.stringify(lane.workflow.jobs).includes("cuenv ci")) && files.some(file => /[\x00-\x1f\x7f"\\]|[^\x00-\x7f]/.test(file))) throw new Error("Changed filename cannot be safely parsed by pinned cuenv");
    appendFileSync(process.env.GITHUB_OUTPUT!, `base=${base}\n` + selected.map(lane => `${lane.id}=true\n`).join("") + `expected=${JSON.stringify(selected.flatMap(jobIds))}\n`);
    console.log(JSON.stringify({ candidate: sha, base, changedFiles: files, workflows: selected.map(lane => lane.file) }, null, 2));
  } else if (mode === "gate") {
    verifyGate(JSON.parse(process.env.JOB_RESULTS!), JSON.parse(process.env.EXPECTED_JOBS!), lanes.flatMap(jobIds));
    console.log("Every applicable verification job passed at this candidate SHA.");
  } else throw new Error("Usage: bun scripts/integration-ci.ts {sync|--check|plan|gate}");
}
