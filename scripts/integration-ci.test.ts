import { describe, expect, test } from "bun:test";
import { applicable, candidateBase, generate, jobIds, readLanes, selectLanes, verifyGate, type Lane } from "./integration-ci";

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
    expect(applicable(lane(["server/**"]), ["scripts/test_cuenv_bootstrap.py"])).toBe(true);
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
    expect(Object.keys(workflow.on).sort()).toEqual(["merge_group", "workflow_dispatch"]);
  });
  test("expensive work runs once in the affected queue graph and never on PR updates", () => {
    const lanes = readLanes();
    const cases = [
      ["operational-qualification", "server/experiments/operational-qualification/Cargo.toml"],
      ["waddle-android-queuechecks", "apps/android/app/src/main/AndroidManifest.xml"],
      ["waddle-apple-default", "apps/apple/WaddleKit/Package.swift"],
      ["waddle-chat-queuechecks", "server/crates/waddle-xmpp-client-wasm/src/lib.rs"],
      ["waddle-linux-queuechecks", "apps/linux/src/main.rs"],
      ["waddle-operator-queuechecks", "operator/crates/waddle-operator/src/main.rs"],
      ["waddle-server-queuechecks", "server/crates/waddle-server/src/lib.rs"],
      ["waddle-server-namespacerustchecks", "server/crates/waddle-server/src/lib.rs"],
      ["waddle-server-xmppcompliance", "server/crates/waddle-xmpp/src/lib.rs"],
      ["waddle-server-federationgrantfence", "server/experiments/grant-fence/src/lib.rs"],
    ];
    for (const [name, changed] of cases) {
      const matches = lanes.filter(lane => lane.file === name + ".yml");
      expect(matches).toHaveLength(1);
      const [lane] = matches;
      expect(Object.keys(lane.workflow.on)).not.toContain("pull_request");
      expect(applicable(lane, [changed]), name + " should cover " + changed).toBe(true);
      expect(applicable(lane, ["website/src/unrelated.astro"])).toBe(false);
      expect(applicable(lane, [".github/workflows/" + lane.file])).toBe(true);
    }
    const rust = lanes.find(lane => lane.file === "waddle-server-namespacerustchecks.yml")!;
    for (const path of ["ci/cache-benchmark/cargo.cue", "scripts/ci-timings.py", "server/.config/required-cluster-tests.txt"]) {
      expect(applicable(rust, [path]), path).toBe(false);
    }
    const serverPr = lanes.find(lane => lane.file === "waddle-server-pullrequest.yml")!;
    expect(Object.keys(serverPr.workflow.jobs)).toEqual(["pullRequestChecks"]);
    expect(lanes.some(lane => Object.hasOwn(lane.workflow.jobs, "publishContainerImage"))).toBe(false);
  });

  test("main covers ordinary namespace tests and keeps fixture, graph and ambiguous changes on the queue", () => {
    const lanes = readLanes();
    const rust = lanes.find(lane => lane.file === "waddle-server-namespacerustchecks.yml")!;
    const source = "server/crates/new-crate/src/lib.rs";
    const fixture = "server/experiments/native-fixture/src/new.rs";
    const queued = (files: string[], main = true) => selectLanes(lanes, files, main).includes(rust);
    for (const path of [source, "server/Cargo.lock", "flake.lock", "infrastructure/waddle.cloud/rules/mimir/new.yaml"]) {
      expect(queued([path]), path).toBe(false);
    }
    expect(queued(["website/src/unrelated.astro"])).toBe(false);
    for (const files of [[fixture], ["server/experiments/grant-fence/Cargo.toml"], [fixture, source],
      [source, "scripts/integration-ci.ts"], [source, "ci/rust-tests/queue-paths.cue"], ["namespace.cue"],
      [".github/workflows/" + rust.file], [".github/workflows/waddle-server-default.yml", source],
      [source, ...Array.from({length: 300}, (_, n) => `new/file-${n}`)]]) {
      expect(queued(files), files[0]).toBe(true);
    }
    expect(queued([source], false)).toBe(true);
    const unsupported = {...rust, paths: ["dir/[a-z]*"]};
    expect(selectLanes([unsupported], [source], true)).toEqual([unsupported]);

    // With no catch-all lane in this graph the unknown path selects all
    // candidates. A demonstrated publisher match can still remove its repeat.
    const unknown = "new-component/unknown.ext";
    expect(selectLanes([rust], [unknown], true)).toEqual([rust]);
    expect(selectLanes([rust], [unknown, source], true)).toEqual([]);
  });

  test("Apple sources stay classified while main runs the Swift, simulator and XCFramework checks", () => {
    const lanes = readLanes();
    const apple = lanes.find(lane => lane.file === "waddle-apple-default.yml")!;
    expect(apple.workflow.concurrency["cancel-in-progress"]).toBe(false);
    const changed = ["apps/apple/WaddleKit/Sources/New.swift"];
    const otherAffected = lanes.filter(lane => lane !== apple && applicable(lane, changed));
    expect(selectLanes(lanes, changed, true)).toEqual(otherAffected);
    expect(selectLanes(lanes, changed, false)).toContain(apple);
    expect(selectLanes(lanes, ["server/crates/waddle-xmpp-core/src/new.rs"], true)).not.toContain(apple);
    expect(apple.workflow.on.workflow_dispatch).toBeDefined();
  });

  test("removing a deferred workflow fails queue planning", () => {
    const directory = mkdtempSync(join(tmpdir(), "waddle-queue-inventory-"));
    try {
      cpSync(".github/workflows", directory, { recursive: true });
      rmSync(join(directory, "waddle-server-queuechecks.yml"));
      expect(() => readLanes(directory)).toThrow("Missing deferred workflow waddle-server-queuechecks");
    } finally { rmSync(directory, { recursive: true, force: true }); }
  });

  test("discovers PR and deferred verification lanes", () => {
    const lanes = readLanes();
    const workflow = Bun.YAML.parse(generate(lanes)) as any;
    for (const name of ["apple", "android", "chat", "federationgrantfence", "namespacerustchecks", "xmppcompliance"]) {
      expect(lanes.some(lane => lane.file.includes(name))).toBe(true);
    }
    // The shard workflow is dispatch-only (no pull_request trigger), so it is not a lane.
    expect(lanes.some(lane => lane.file.includes("rusttests"))).toBe(false);
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


describe("immutable generated action references", () => {
  const collect = (value: any): string[] => {
    if (Array.isArray(value)) return value.flatMap(collect);
    if (!value || typeof value !== "object") return [];
    return Object.entries(value).flatMap(([key, item]) => key === "uses" ? [item as string] : collect(item));
  };
  test("pins known mutable aliases and retains existing full SHAs", () => {
    const source = lane();
    const preserved = "example/action/subpath@" + "A".repeat(40);
    source.workflow.jobs.build.steps.push({ uses: "actions/setup-python@v5" }, { uses: preserved });
    const workflow = Bun.YAML.parse(generate([source])) as any;
    expect(workflow.jobs.example__build.steps[0].uses).toBe("actions/checkout@11d5960a326750d5838078e36cf38b85af677262");
    expect(workflow.jobs.example__build.steps[2].uses).toBe("actions/setup-python@a26af69be951a213d495a4c3e4e4022e16d87065");
    expect(workflow.jobs.example__build.steps[3].uses).toBe(preserved);
    source.workflow.jobs.build.steps[0].uses = "actions/checkout@v7";
    const next = Bun.YAML.parse(generate([source])) as any;
    expect(next.jobs.example__build.steps[0].uses).toBe("actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1");
  });
  test("every action in the complete generated graph uses a full commit SHA", () => {
    const lanes = readLanes();
    const references = collect(Bun.YAML.parse(generate(lanes)));
    expect(references.length).toBeGreaterThan(30);
    for (const reference of references) expect(reference).toMatch(/^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*(?:\/[A-Za-z0-9_][A-Za-z0-9_.-]*)*@[0-9a-fA-F]{40}$/);
    for (const reference of collect(lanes).filter(reference => /@[0-9a-fA-F]{40}$/.test(reference) && !reference.startsWith("DeterminateSystems/flakehub-cache-action@"))) expect(references).toContain(reference);
  });
  test("rejects unknown mutable, shortened, local, container and expression references", () => {
    for (const reference of ["actions/checkout@v8", "actions/checkout@v5", "actions/setup-python@main", "unknown/action@v1", "unknown/action@abcdef0", "./candidate-action", "./candidate-action@" + "a".repeat(40), "docker://example:latest", "${{ inputs.action }}"]) {
      const source = lane();
      source.workflow.jobs.build.steps.push({ uses: reference });
      expect(() => generate([source])).toThrow("Unreviewed mutable action reference");
    }
  });
});

import { cpSync, mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, renameSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

test("every ordinary PR has exactly one unfiltered read-only smoke job", () => {
  const workflows = ".github/workflows";
  const pullRequests = readdirSync(workflows).filter(file => {
    const { on } = Bun.YAML.parse(readFileSync(join(workflows, file), "utf8"));
    return Array.isArray(on) ? on.includes("pull_request") : on === "pull_request" || Object.hasOwn(on, "pull_request");
  });
  expect(pullRequests).toEqual(["waddle-pr-smoke.yml"]);
  const smoke = Bun.YAML.parse(readFileSync(join(workflows, pullRequests[0]), "utf8"));
  expect(smoke.on.pull_request ?? {}).toEqual({});
  expect(smoke.on.merge_group).toEqual({ types: ["checks_requested"] });
  expect(smoke.permissions).toEqual({ contents: "read" });
  expect(Object.keys(smoke.jobs)).toEqual(["smoke"]);
  expect(smoke.jobs.smoke["runs-on"]).toBe("ubuntu-24.04");
  expect(readLanes().some(lane => lane.file === pullRequests[0])).toBe(false);
});

test("the smoke checks the exact diff, filenames and strict JSON", () => {
  const directory = mkdtempSync(join(tmpdir(), "waddle-smoke-"));
  const script = resolve("scripts/pr-smoke.ts");
  const run = (args: string[]) => Bun.spawnSync(args, { cwd: directory });
  const git = (...args: string[]) => {
    const result = run(["git", "-c", "commit.gpgsign=false", "-c", "user.name=Test", "-c", "user.email=test@example.test", ...args]);
    expect(result.exitCode).toBe(0);
    return result.stdout.toString().trim();
  };
  try {
    git("init", "-q");
    git("commit", "--allow-empty", "-qm", "base");
    const base = git("rev-parse", "HEAD");
    writeFileSync(join(directory, "-a\nfile.sh"), "true\n");
    writeFileSync(join(directory, "sample.json"), "{}\n");
    git("add", ".");
    git("commit", "-qm", "valid syntax");
    expect(run([process.execPath, script, base, git("rev-parse", "HEAD")]).exitCode).toBe(0);
    writeFileSync(join(directory, "sample.json"), "{\"trailing\":1,}\n");
    git("commit", "-qam", "invalid syntax");
    expect(run([process.execPath, script, base, git("rev-parse", "HEAD")]).exitCode).not.toBe(0);
    expect(run([process.execPath, script, base, base]).exitCode).not.toBe(0);
    // The event saw the old target. The synthetic merge includes a newer target
    // with its own broken JSON. Only changes from the PR itself belong in smoke.
    const newerBase = git("rev-parse", "HEAD");
    git("checkout", "-qb", "contribution", base);
    writeFileSync(join(directory, "changed.py"), "value = 42\n");
    git("add", "changed.py");
    git("commit", "-qm", "contribution");
    const contribution = git("rev-parse", "HEAD");
    git("checkout", "-q", "--detach", newerBase);
    git("merge", "--no-ff", "-qm", "synthetic candidate", contribution);
    const merged = git("rev-parse", "HEAD");
    expect(run([process.execPath, script, base, merged, contribution]).exitCode).toBe(0);
    expect(run([process.execPath, script, base, merged, newerBase]).exitCode).not.toBe(0);

  } finally { rmSync(directory, { recursive: true, force: true }); }
});

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
    cpSync(".github/workflows", join(directory, ".github/workflows"), { recursive: true });
    writeFileSync(join(directory, ".github/workflows/example.yml"), Bun.YAML.stringify(lane(["old/**"]).workflow));
    mkdirSync(join(directory, "old"));
    writeFileSync(join(directory, "old/a\nfile.rs"), "unchanged payload");
    git("init", "-q");
    git("config", "user.name", "Integration contracts");
    git("config", "user.email", "integration@example.test");
    git("add", "."); git("commit", "-qm", "base");
    const base = git("rev-parse", "HEAD");
    mkdirSync(join(directory, "server/crates/new"), { recursive: true });
    renameSync(join(directory, "old/a\nfile.rs"), join(directory, "server/crates/new/a\nfile.rs"));
    git("add", "-A"); git("commit", "-qm", "rename");
    const sha = git("rev-parse", "HEAD");
    const eventPath = join(directory, "event.json"), outputPath = join(directory, "output");
    writeFileSync(eventPath, JSON.stringify({ action: "checks_requested", merge_group: { head_sha: sha, base_sha: base } }));
    const env = { GITHUB_EVENT_NAME: "merge_group", GITHUB_EVENT_PATH: eventPath, GITHUB_SHA: sha, GITHUB_OUTPUT: outputPath };
    const result = run([process.execPath, script, "plan"], env);
    expect(result.exitCode).toBe(0);
    const expected = readFileSync(outputPath, "utf8").split("\n").find(line => line.startsWith("expected="))!;
    expect(JSON.parse(expected.slice("expected=".length)).filter((id: string) => id.startsWith("example__"))).toEqual(["example__build", "example__verify"]);
    const report = JSON.parse(result.stdout.toString());
    expect(report.changedFiles).toEqual(["old/a\nfile.rs", "server/crates/new/a\nfile.rs"]);
    expect(report.workflows).toContain("waddle-server-namespacerustchecks.yml");
    writeFileSync(eventPath, JSON.stringify({ action: "checks_requested", merge_group: {
      head_sha: sha, base_sha: base, base_ref: "refs/heads/main",
    } }));
    const thin = lane(["old/**"]);
    thin.workflow.jobs.build.steps.push({ run: "cuenv ci --pipeline pullRequest --path chat" });
    writeFileSync(join(directory, ".github/workflows/example.yml"), Bun.YAML.stringify(thin.workflow));
    const encodedPath = run([process.execPath, script, "plan"], env);
    expect(encodedPath.exitCode).toBe(0);
    expect(JSON.parse(encodedPath.stdout.toString()).workflows).not.toContain("waddle-server-namespacerustchecks.yml");
    expect(JSON.parse(encodedPath.stdout.toString()).changedFiles).toEqual(["old/a\nfile.rs", "server/crates/new/a\nfile.rs"]);
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

test("thin pipelines validate exact revisions and attest complete original CI execution", () => {
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
    const testCommand = command.replace(/^WADDLE_TEST_ALL_CUENV=1[^\n]+\n/m, "").replace(/python3 scripts\/integration-ci-project\.py[^\n]+$/, `printf called > '${marker}'`);
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


