import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
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

function run(workspace: string, path: string) {
	return spawnSync("bash", [guard], {
		encoding: "utf8",
		env: { ...process.env, GITHUB_WORKSPACE: workspace, PATH: path },
	});
}

afterEach(() => {
	for (const value of directories.splice(0)) rmSync(value, { recursive: true, force: true });
});

describe("root sync guard launcher", () => {
	test("missing or non-executable cuenv fails without a marker", () => {
		for (const executable of [false, true]) {
			const workspace = directory();
			const bin = join(workspace, "bin");
			mkdirSync(bin, { recursive: true });
			Bun.write(join(bin, "cuenv"), "#!/usr/bin/env bash\nexit 0\n");
			if (executable) chmodSync(join(bin, "cuenv"), 0o644);
			const result = run(workspace, `${bin}:/usr/bin:/bin`);
			expect(result.status).not.toBe(0);
			expect(existsSync(join(workspace, "ci/root-sync/.root-sync-guard-passed"))).toBe(false);
		}
	});

	test("uses the absolute executable and creates the marker only after the guard command", () => {
		const workspace = directory();
		const bin = join(workspace, "bin");
		const calls = join(workspace, "calls");
		mkdirSync(join(workspace, "ci", "root-sync"), { recursive: true });
		mkdirSync(bin, { recursive: true });
		Bun.write(
			join(bin, "cuenv"),
			`#!/usr/bin/env bash\nprintf '%s\\n' "$@" > ${JSON.stringify(calls)}\nexit 0\n`,
		);
		chmodSync(join(bin, "cuenv"), 0o755);
		const result = run(workspace, `${bin}:/usr/bin:/bin`);
		expect(result.status).toBe(0);
		expect(readFileSync(calls, "utf8")).toContain(`-p\n${workspace}/ci/root-sync`);
		expect(readFileSync(calls, "utf8")).toContain(`ROOT_SYNC_CUENV=${bin}/cuenv`);
		expect(existsSync(join(workspace, "ci/root-sync/.root-sync-guard-passed"))).toBe(true);
	});

	test("keeps one direct guard and one marker anchor in each generated workflow", () => {
		for (const name of ["waddle-root-sync-default.yml", "waddle-root-sync-pullrequest.yml"]) {
			const workflow = readFileSync(join(repositoryRoot, ".github/workflows", name), "utf8");
			expect(workflow.match(/Run root sync guard/gu)).toHaveLength(1);
			expect(workflow.match(/- name: requireGuardMarker/gu)).toHaveLength(1);
			expect(workflow.match(/Setup cuenv \(release\)/gu)).toHaveLength(1);
			expect(workflow.match(/DeterminateSystems\/determinate-nix-action/gu)).toHaveLength(1);
		}
	});
});
