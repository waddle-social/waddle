import { describe, expect, test } from "bun:test";
import { applicable, candidateBase, generate, jobIds, readLanes, verifyGate, type Lane } from "./integration-ci";

const lane = (paths?: string[]): Lane => ({ id: "example", file: "example.yml", workflow: {
  name: "example", on: { pull_request: paths ? { paths } : {} }, jobs: {
    build: { "runs-on": ["large-32-core"], permissions: { contents: "read" }, steps: [{ uses: "actions/checkout@v4" }, { run: "build" }] },
    verify: { "runs-on": "ubuntu-24.04", needs: ["build"], if: "${{ always() }}", steps: [{ run: "echo result", env: { RESULT: "${{ needs.build.result }}" } }] },
  },
} });

describe("path coverage", () => {
  test("retains nested, root, hidden, deleted and both renamed paths", () => {
    expect(applicable(lane(["server/**"]), ["server/src/a.rs"])).toBe(true);
    expect(applicable(lane(["*.lock"]), ["bun.lock"])).toBe(true);
    expect(applicable(lane([".cargo/**"]), [".cargo/config.toml"])).toBe(true);
    expect(applicable(lane(["old/**"]), ["old/deleted.rs", "new/renamed.rs"])).toBe(true);
    expect(applicable(lane(["new/**"]), ["old/deleted.rs", "new/renamed.rs"])).toBe(true);
    expect(applicable(lane(["server/**"]), ["docs/a.md"])).toBe(false);
  });
  test("unknown syntax selects coverage conservatively", () => {
    for (const pattern of ["!server/**", "server/+foo", "[abc]", "a{b,c}"]) expect(applicable(lane([pattern]), ["other/file"])).toBe(true);
  });
  test("unfiltered lanes and graph changes are always selected", () => {
    expect(applicable(lane(), ["docs/a.md"])).toBe(true);
    expect(applicable(lane(["server/**"]), ["scripts/integration-ci.ts"])).toBe(true);
  });
});

describe("candidate identity", () => {
  const sha = "a".repeat(40), base = "b".repeat(40);
  test("merge group must bind head and action", () => {
    expect(candidateBase("merge_group", { action: "checks_requested", merge_group: { head_sha: sha, base_sha: base } }, sha)).toBe(base);
    expect(() => candidateBase("merge_group", { action: "checks_requested", merge_group: { head_sha: base, base_sha: base } }, sha)).toThrow();
    expect(() => candidateBase("merge_group", { action: "destroyed", merge_group: { head_sha: sha, base_sha: base } }, sha)).toThrow();
  });
  test("rejects missing, malformed, and unsupported event metadata", () => {
    for (const event of [{}, { inputs: { base_sha: "main" } }]) expect(() => candidateBase("workflow_dispatch", event, sha)).toThrow();
    expect(() => candidateBase("push", {}, sha)).toThrow();
    expect(() => candidateBase("pull_request", {}, sha)).toThrow();
    expect(candidateBase("workflow_dispatch", { inputs: { base_sha: base } }, sha)).toBe(base);
  });
});

describe("aggregate", () => {
  const jobs = ["a", "b"];
  test("requires success for every applicable job and skipped for others", () => {
    expect(() => verifyGate({ plan: { result: "success" }, a: { result: "success" }, b: { result: "skipped" } }, ["a"], jobs)).not.toThrow();
    for (const result of ["skipped", "failure", "cancelled", "neutral", ""]) {
      expect(() => verifyGate({ plan: { result: "success" }, a: { result }, b: { result: "skipped" } }, ["a"], jobs)).toThrow();
    }
  });
  test("fails closed on planner failure, omissions and corrupt inventory", () => {
    expect(() => verifyGate({ plan: { result: "failure" } }, [], jobs)).toThrow();
    expect(() => verifyGate({ plan: { result: "success" } }, ["a"], jobs)).toThrow();
    expect(() => verifyGate({ plan: { result: "success" } }, ["unknown"], jobs)).toThrow();
    expect(() => verifyGate({ plan: { result: "success" } }, ["a", "a"], jobs)).toThrow();
  });
});

describe("graph parity", () => {
  test("preserves commands, runner resources, permissions and dependency results", () => {
    const workflow = Bun.YAML.parse(generate([lane(["server/**"])])) as any;
    expect(workflow.jobs.example__build["runs-on"]).toEqual(["large-32-core"]);
    expect(workflow.jobs.example__build.steps[1].run).toBe("build");
    expect(workflow.jobs.example__build.steps[0].with.ref).toBe("${{ github.sha }}");
    expect(workflow.jobs.example__verify.needs).toEqual(["plan", "example__build"]);
    expect(workflow.jobs.example__verify.steps[0].env.RESULT).toBe("${{ needs.example__build.result }}");
    expect(workflow.jobs.example__verify.if).toContain("always()");
    expect(workflow.jobs.integration.if).toBe("${{ always() }}");
    expect(workflow.jobs.integration.needs).toEqual(["plan", "example__build", "example__verify"]);
    expect(workflow.on.merge_group.types).toEqual(["checks_requested"]);
    expect(workflow.on.pull_request).toEqual({});
  });
  test("discovers all existing PR lanes, including experiment and native lanes", () => {
    const lanes = readLanes();
    const workflow = Bun.YAML.parse(generate(lanes)) as any;
    for (const name of ["apple", "android", "chat", "federationgrantfence", "rusttests", "namespacerustchecks", "xmppservercompliance"]) {
      expect(lanes.some(lane => lane.file.includes(name))).toBe(true);
    }
    expect(Object.keys(workflow.jobs).sort()).toEqual(["plan", "integration", ...lanes.flatMap(jobIds)].sort());
    for (const lane of lanes) {
      for (const [id, source] of Object.entries(lane.workflow.jobs) as [string, any][]) {
        const generated = workflow.jobs[`${lane.id}__${id.toLowerCase()}`];
        expect(generated["runs-on"]).toEqual(source["runs-on"]);
        expect(generated.permissions).toEqual(source.steps.some((step: any) => step.uses?.startsWith("actions/checkout@")) ? { contents: "read" } : {});
        expect(JSON.stringify(generated)).not.toContain("secrets.");
        expect(generated.steps.some((step: any) => step.uses?.startsWith("DeterminateSystems/flakehub-cache-action@"))).toBe(false);
        expect(generated.steps.filter((step: any) => step.run).map((step: any) => (step.env?.WADDLE_CI_BASE_SHA ? step.run.split("\n").at(-1).replace(/^python3 scripts\/integration-ci-project\.py --path (\S+) --pipeline (\S+) --run$/, "cuenv ci --pipeline $2 --path $1") : step.run))).toEqual(source.steps.filter((step: any) => step.run).map((step: any) => step.run));
      }
    }
  });
});

import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, renameSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

test("planner executes against exact git objects, NUL-safe renames, and missing bases", () => {
  const directory = mkdtempSync(join(tmpdir(), "waddle-integration-contract-"));
  const script = resolve("scripts/integration-ci.ts");
  const run = (args: string[], env = {}) => Bun.spawnSync(args, { cwd: directory, env: { ...process.env, ...env } });
  const git = (...args: string[]) => {
    const result = run(["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", ...args]);
    expect(result.exitCode).toBe(0);
    return result.stdout.toString().trim();
  };
  try {
    mkdirSync(join(directory, ".github/workflows"), { recursive: true });
    writeFileSync(join(directory, ".github/workflows/example.yml"), Bun.YAML.stringify(lane(["old/**"]).workflow));
    mkdirSync(join(directory, "old"));
    writeFileSync(join(directory, "old/a\nfile.rs"), "unchanged payload");
    git("init", "-q");
    git("config", "user.name", "Integration contracts");
    git("config", "user.email", "integration@example.test");
    git("add", "."); git("commit", "-qm", "base");
    const base = git("rev-parse", "HEAD");
    mkdirSync(join(directory, "new"));
    renameSync(join(directory, "old/a\nfile.rs"), join(directory, "new/a\nfile.rs"));
    git("add", "-A"); git("commit", "-qm", "rename");
    const sha = git("rev-parse", "HEAD");
    const eventPath = join(directory, "event.json"), outputPath = join(directory, "output");
    writeFileSync(eventPath, JSON.stringify({ action: "checks_requested", merge_group: { head_sha: sha, base_sha: base } }));
    const env = { GITHUB_EVENT_NAME: "merge_group", GITHUB_EVENT_PATH: eventPath, GITHUB_SHA: sha, GITHUB_OUTPUT: outputPath };
    const result = run([process.execPath, script, "plan"], env);
    expect(result.exitCode).toBe(0);
    expect(readFileSync(outputPath, "utf8")).toContain('expected=["example__build","example__verify"]');
    const report = JSON.parse(result.stdout.toString());
    expect(report.changedFiles).toEqual(["new/a\nfile.rs", "old/a\nfile.rs"]);
    const thin = lane(["old/**"]);
    thin.workflow.jobs.build.steps.push({ run: "cuenv ci --pipeline pullRequest --path chat" });
    writeFileSync(join(directory, ".github/workflows/example.yml"), Bun.YAML.stringify(thin.workflow));
    const encodedPath = run([process.execPath, script, "plan"], env);
    expect(encodedPath.exitCode).toBe(0);
    expect(JSON.parse(encodedPath.stdout.toString()).changedFiles).toEqual(["new/a\nfile.rs", "old/a\nfile.rs"]);
    writeFileSync(eventPath, JSON.stringify({ action: "checks_requested", merge_group: { head_sha: sha, base_sha: "f".repeat(40) } }));
    expect(run([process.execPath, script, "plan"], env).exitCode).not.toBe(0);
    writeFileSync(eventPath, JSON.stringify({ action: "checks_requested", merge_group: { head_sha: sha, base_sha: sha } }));
    expect(run([process.execPath, script, "plan"], env).exitCode).not.toBe(0);
    expect(run([process.execPath, script, "plan"], { ...env, GITHUB_SHA: base }).exitCode).not.toBe(0);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test("duplicate job identifiers fail rather than overwrite coverage", () => {
  expect(() => generate([lane(), lane()])).toThrow("Duplicate");
});

test("candidate jobs cannot inherit check writes, PR writes, OIDC or explicit GitHub credentials", () => {
  const source = lane();
  source.workflow.jobs.build.permissions = { contents: "read", checks: "write", "pull-requests": "write", "id-token": "write" };
  source.workflow.jobs.build.env = { GH_TOKEN: "${{ secrets.GITHUB_TOKEN }}" };
  source.workflow.jobs.build.steps.push({ uses: "DeterminateSystems/flakehub-cache-action@pinned" });
  source.workflow.jobs.build.steps.push({ run: "verify", env: { GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}" } });
  const workflow = Bun.YAML.parse(generate([source])) as any;
  const job = workflow.jobs.example__build;
  expect(job.permissions).toEqual({ contents: "read" });
  expect(job.env).toBeUndefined();
  expect(job.steps[0].with["persist-credentials"]).toBe(false);
  expect(job.steps[2].run).toBe("verify");
  expect(job.steps[2].env).toBeUndefined();
  expect(JSON.stringify(job)).not.toContain("flakehub-cache-action");
  expect(JSON.stringify(job)).not.toContain("secrets.");
  expect(workflow.jobs.example__verify.permissions).toEqual({});
});

test("new unreviewed secret use fails generation", () => {
  const source = lane();
  source.workflow.jobs.build.steps.push({ run: "verify", env: { PRIVATE_TOKEN: "${{ secrets.PRIVATE_TOKEN }}" } });
  expect(() => generate([source])).toThrow("Unreviewed secret");
});

test("thin pipelines validate exact revisions and execute all attested named tasks", () => {
  const source = lane(["chat/**"]);
  source.workflow.jobs.build.steps.push({ run: "cuenv ci --pipeline pullRequest --path chat" });
  const workflow = Bun.YAML.parse(generate([source])) as any;
  const job = workflow.jobs.example__build;
  expect(job.steps[0].with["fetch-depth"]).toBe(0);
  const step = job.steps[2];
  expect(step.run.split("\n").at(-1)).toBe("python3 scripts/integration-ci-project.py --path chat --pipeline pullRequest --run");
  expect(step.env.WADDLE_CI_BASE_SHA).toBe("${{ needs.plan.outputs.base }}");
  expect(step.env.WADDLE_CI_HEAD_SHA).toBe("${{ github.sha }}");
  expect(workflow.jobs.plan.outputs.base).toBe("${{ steps.plan.outputs.base }}");
});

test("each thin runner fails before cuenv on a missing base, changed HEAD or failed diff", () => {
  const directory = mkdtempSync(join(tmpdir(), "waddle-thin-preflight-"));
  const source = lane(["chat/**"]);
  source.workflow.jobs.build.steps.push({ run: "cuenv ci --pipeline pullRequest --path chat" });
  const workflow = Bun.YAML.parse(generate([source])) as any;
  const command = workflow.jobs.example__build.steps[2].run;
  const run = (args: string[], env = {}) => Bun.spawnSync(args, { cwd: directory, env: { ...process.env, ...env } });
  const git = (...args: string[]) => {
    const result = run(["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", ...args]);
    expect(result.exitCode).toBe(0);
    return result.stdout.toString().trim();
  };
  try {
    git("init", "-q"); git("config", "user.name", "Integration contracts"); git("config", "user.email", "integration@example.test");
    writeFileSync(join(directory, "file"), "base"); git("add", "."); git("commit", "-qm", "base");
    const base = git("rev-parse", "HEAD");
    const baseTree = git("rev-parse", "HEAD^{tree}");
    writeFileSync(join(directory, "file"), "head"); git("add", "."); git("commit", "-qm", "head");
    const head = git("rev-parse", "HEAD");
    const marker = join(directory, "cuenv-called");
    const testCommand = command.replace(/^WADDLE_TEST_PINNED_CUENV=1[^\n]+\n/m, "").replace(/python3 scripts\/integration-ci-project\.py[^\n]+$/, `printf called > '${marker}'`);
    const env = { WADDLE_CI_BASE_SHA: base, WADDLE_CI_HEAD_SHA: head };
    expect(run(["bash", "-c", testCommand], env).exitCode).toBe(0);
    expect(readFileSync(marker, "utf8")).toBe("called");
    rmSync(marker);
    expect(run(["bash", "-c", testCommand], { ...env, WADDLE_CI_BASE_SHA: "f".repeat(40) }).exitCode).not.toBe(0);
    expect(run(["bash", "-c", testCommand], { ...env, WADDLE_CI_HEAD_SHA: base }).exitCode).not.toBe(0);
    const outside = mkdtempSync(join(tmpdir(), "waddle-no-repository-"));
    try {
      expect(Bun.spawnSync(["bash", "-c", testCommand], { cwd: outside, env: { ...process.env, ...env } }).exitCode).not.toBe(0);
    } finally { rmSync(outside, { recursive: true, force: true }); }
    // Corrupt the base tree: commit availability and ancestry still pass, but
    // the real complete diff fails. cuenv must never see that as zero tasks.
    rmSync(join(directory, ".git/objects", baseTree.slice(0, 2), baseTree.slice(2)));
    expect(run(["bash", "-c", testCommand], env).exitCode).not.toBe(0);
    expect(() => readFileSync(marker, "utf8")).toThrow();
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
