import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const directories: string[] = [];
const repositoryRoot = resolve(import.meta.dir, "..", "..");
const guard = join(import.meta.dir, "run-root-sync-guard.sh");

function directory() {
	const value = mkdtempSync(join(tmpdir(), "waddle-root-sync-guard-"));
	directories.push(value);
	return value;
}

function write(path: string, contents: string, mode?: number) {
	mkdirSync(resolve(path, ".."), { recursive: true });
	writeFileSync(path, contents);
	if (mode !== undefined) chmodSync(path, mode);
}

function run(workspace: string, path: string) {
	return spawnSync("bash", [guard], {
		encoding: "utf8",
		env: { ...process.env, GITHUB_WORKSPACE: workspace, PATH: path },
	});
}

function marker(workspace: string) {
	return join(workspace, "ci", "root-sync", ".root-sync-guard-passed");
}

function runAnchor(workspace: string) {
	return spawnSync(
		"bash",
		["-ceu", 'marker=.root-sync-guard-passed; test -f "$marker"; rm -- "$marker"; test ! -e "$marker"'],
		{ cwd: join(workspace, "ci", "root-sync"), encoding: "utf8" },
	);
}

afterEach(() => {
	for (const value of directories.splice(0)) rmSync(value, { recursive: true, force: true });
});

describe("root sync guard launcher", () => {
	test("missing cuenv fails closed without a marker", () => {
		const workspace = directory();
		const result = run(workspace, "/usr/bin:/bin");
		expect(result.status).not.toBe(0);
		expect(existsSync(marker(workspace))).toBe(false);
	});

	test("a present mode 0644 cuenv fails closed without a marker", () => {
		const workspace = directory();
		const bin = join(workspace, "bin");
		write(join(bin, "cuenv"), "#!/usr/bin/env bash\nexit 0\n", 0o644);
		const result = run(workspace, `${bin}:/usr/bin:/bin`);
		expect(result.status).not.toBe(0);
		expect(existsSync(marker(workspace))).toBe(false);
	});

	test("an executable cuenv failure retains a nonblank diagnostic and no marker", () => {
		const workspace = directory();
		const bin = join(workspace, "bin");
		write(join(bin, "cuenv"), "#!/usr/bin/env bash\necho cuenv-child-failed >&2\nexit 7\n", 0o755);
		write(join(bin, "bun"), "#!/usr/bin/env bash\nexit 0\n", 0o755);
		const result = run(workspace, `${bin}:/usr/bin:/bin`);
		expect(result.status).toBe(7);
		expect(result.stderr).toContain("cuenv-child-failed");
		expect(existsSync(marker(workspace))).toBe(false);
	});

	test("executes launcher test then server contract then guard and marks last", () => {
		const workspace = directory();
		const bin = join(workspace, "bin");
		const order = join(workspace, "order");
		write(
			join(bin, "cuenv"),
			`#!/usr/bin/env bash\nprintf '%s\\n' "$@" > ${JSON.stringify(join(workspace, "argv"))}\nexec "${"${@:6}"}"\n`,
			0o755,
		);
		write(
			join(bin, "bun"),
			`#!/usr/bin/env bash\nif [ "$1" = test ] && [ "$2" = ci/root-sync/run-root-sync-guard.test.ts ]; then echo launcher-test >> ${JSON.stringify(order)}; elif [ "$1" = test ]; then echo server-contract >> ${JSON.stringify(order)}; else echo actual-guard >> ${JSON.stringify(order)}; fi\n`,
			0o755,
		);
		mkdirSync(join(workspace, "ci", "root-sync"), { recursive: true });
		mkdirSync(join(workspace, "server", "scripts"), { recursive: true });
		const result = run(workspace, `${bin}:/usr/bin:/bin`);
		expect(result.status).toBe(0);
		expect(readFileSync(join(workspace, "argv"), "utf8")).toContain(`-p\n${workspace}/ci/root-sync`);
		expect(readFileSync(join(workspace, "argv"), "utf8")).toContain(`ROOT_SYNC_CUENV=${bin}/cuenv`);
		expect(readFileSync(order, "utf8")).toBe("launcher-test\nserver-contract\nactual-guard\n");
		expect(existsSync(marker(workspace))).toBe(true);
	});

	test("a nonblank guarded child failure leaves no marker", () => {
		const workspace = directory();
		const bin = join(workspace, "bin");
		write(join(bin, "cuenv"), "#!/usr/bin/env bash\nexec \"${@:6}\"\n", 0o755);
		write(
			join(bin, "bun"),
			'#!/usr/bin/env bash\nif [ "$1" != test ]; then echo guarded-child-failed >&2; exit 9; fi\n',
			0o755,
		);
		mkdirSync(join(workspace, "ci", "root-sync"), { recursive: true });
		mkdirSync(join(workspace, "server", "scripts"), { recursive: true });
		const result = run(workspace, `${bin}:/usr/bin:/bin`);
		expect(result.status).toBe(9);
		expect(result.stderr).toContain("guarded-child-failed");
		expect(existsSync(marker(workspace))).toBe(false);
	});

	test("the marker anchor requires then removes its marker and remains non-cacheable", () => {
		const source = readFileSync(join(repositoryRoot, "ci", "root-sync", "env.cue"), "utf8");
		expect(source).toContain('hermetic: false');
		expect(source).toContain('cache: mode: "never"');
		expect(source).toContain('test -f "$marker"');
		expect(source).toContain('rm -- "$marker"');
	});

	test("the marker anchor fails absent and succeeds while removing a present marker", () => {
		const workspace = directory();
		mkdirSync(join(workspace, "ci", "root-sync"), { recursive: true });
		expect(runAnchor(workspace).status).not.toBe(0);
		write(marker(workspace), "passed\n");
		expect(runAnchor(workspace).status).toBe(0);
		expect(existsSync(marker(workspace))).toBe(false);
	});

	test("keeps one job, setup pair, direct guard, and anchor per generated workflow", () => {
		for (const [name, trigger] of [["waddle-root-sync-default.yml", "push:"], ["waddle-root-sync-pullrequest.yml", "pull_request:"]] as const) {
			const workflow = readFileSync(join(repositoryRoot, ".github/workflows", name), "utf8");
			expect(workflow).toContain(trigger);
			expect(workflow.match(/^  requireGuardMarker:/gmu)).toHaveLength(1);
			expect(workflow.match(/Run root sync guard/gu)).toHaveLength(1);
			expect(workflow.match(/- name: requireGuardMarker/gu)).toHaveLength(1);
			expect(workflow.match(/Setup cuenv \(release\)/gu)).toHaveLength(1);
			expect(workflow.match(/DeterminateSystems\/determinate-nix-action/gu)).toHaveLength(1);
			expect(workflow).toContain("contents: read");
			expect(workflow).toContain("checks: none");
			expect(workflow).toContain("pull-requests: none");
		}
	});

	test("runs launcher coverage before the server contract and guard", () => {
		const script = readFileSync(join(repositoryRoot, "ci", "root-sync", "run-root-sync-guard.sh"), "utf8");
		expect(script.indexOf("bun test ci/root-sync/run-root-sync-guard.test.ts")).toBeLessThan(
			script.indexOf("bun test scripts/check-root-sync-drift.test.ts"),
		);
		expect(script.indexOf("bun test scripts/check-root-sync-drift.test.ts")).toBeLessThan(
			script.indexOf("bun scripts/check-root-sync-drift.mjs"),
		);
	});
});
