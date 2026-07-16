import { afterEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import {
	mkdirSync,
	mkdtempSync,
	readFileSync,
	renameSync,
	rmSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { checkCiDrift } from "./check-ci-drift.mjs";

const tempDirectories: string[] = [];
const remediation = `Regenerate the workflows and commit every generated change:
  cuenv sync ci -A
Commit all generated workflow changes under .github/workflows.`;

function runGit(cwd: string, args: string[]): void {
	const result = spawnSync("git", args, {
		cwd,
		encoding: "utf8",
		env: { ...process.env, LC_ALL: "C" },
	});
	if (result.error || result.status !== 0) {
		throw new Error(
			`git ${args.join(" ")} failed (${String(result.status)}): ${result.error?.message ?? result.stderr}`,
		);
	}
}

function createTempDirectory(label: string): string {
	const directory = mkdtempSync(join(tmpdir(), `waddle-ci-drift-${label}-`));
	tempDirectories.push(directory);
	return directory;
}

function createRepository(): string {
	const repository = createTempDirectory("repo");
	runGit(repository, ["init", "--quiet", "--initial-branch=main"]);
	runGit(repository, ["config", "user.name", "Waddle CI Test"]);
	runGit(repository, ["config", "user.email", "ci-test@waddle.invalid"]);
	runGit(repository, ["config", "commit.gpgsign", "false"]);
	mkdirSync(join(repository, ".github", "workflows"), { recursive: true });
	writeFileSync(join(repository, ".github", "workflows", "base.yml"), "name: base\n");
	writeFileSync(join(repository, "outside.txt"), "outside\n");
	runGit(repository, ["add", "."]);
	runGit(repository, ["commit", "--quiet", "-m", "test: seed repository"]);
	return repository;
}

function driftOutput(...records: string[]): string {
	return `Generated GitHub workflow drift detected:\n${records.join("\n")}\n\n${remediation}\n`;
}

function workflowJob(contents: string, name: string): string {
	const marker = `  ${name}:\n`;
	const start = contents.indexOf(marker);
	if (start < 0) return "";

	const bodyStart = start + marker.length;
	const nextJobOffset = contents.slice(bodyStart).search(/\n  [A-Za-z0-9_-]+:\n/u);
	const end = nextJobOffset < 0 ? contents.length : bodyStart + nextJobOffset;
	return contents.slice(start, end);
}

afterEach(() => {
	for (const directory of tempDirectories.splice(0)) {
		rmSync(directory, { recursive: true, force: true });
	}
});

describe("generated GitHub workflow drift check", () => {
	test("passes a clean repository", () => {
		const repository = createRepository();

		expect(checkCiDrift({ cwd: repository })).toEqual({
			exitCode: 0,
			stdout: "",
			stderr: "",
		});
	});

	test("reports an unstaged tracked rewrite with exact remediation", () => {
		const repository = createRepository();
		writeFileSync(join(repository, ".github", "workflows", "base.yml"), "name: changed\n");

		expect(checkCiDrift({ cwd: repository })).toEqual({
			exitCode: 1,
			stdout: "",
			stderr: driftOutput(" M .github/workflows/base.yml"),
		});
	});

	test("reports a tracked deletion", () => {
		const repository = createRepository();
		unlinkSync(join(repository, ".github", "workflows", "base.yml"));

		expect(checkCiDrift({ cwd: repository }).stderr).toBe(
			driftOutput(" D .github/workflows/base.yml"),
		);
	});

	test("reports an untracked workflow nested below the generated directory", () => {
		const repository = createRepository();
		mkdirSync(join(repository, ".github", "workflows", "nested"));
		writeFileSync(join(repository, ".github", "workflows", "nested", "new.yml"), "name: new\n");

		expect(checkCiDrift({ cwd: repository }).stderr).toBe(
			driftOutput("?? .github/workflows/nested/new.yml"),
		);
	});

	test("reports a staged workflow change", () => {
		const repository = createRepository();
		writeFileSync(join(repository, ".github", "workflows", "base.yml"), "name: staged\n");
		runGit(repository, ["add", ".github/workflows/base.yml"]);

		expect(checkCiDrift({ cwd: repository }).stderr).toBe(
			driftOutput("M  .github/workflows/base.yml"),
		);
	});

	test("reports a tracked workflow rename", () => {
		const repository = createRepository();
		renameSync(
			join(repository, ".github", "workflows", "base.yml"),
			join(repository, ".github", "workflows", "renamed.yml"),
		);
		runGit(repository, ["add", "--all", ".github/workflows"]);

		expect(checkCiDrift({ cwd: repository }).stderr).toBe(
			driftOutput(
				"R  .github/workflows/base.yml -> .github/workflows/renamed.yml",
			),
		);
	});

	test("sorts multiple meaningful workflow records deterministically", () => {
		const repository = createRepository();
		writeFileSync(join(repository, ".github", "workflows", "z.yml"), "name: z\n");
		writeFileSync(join(repository, ".github", "workflows", "a.yml"), "name: a\n");
		writeFileSync(join(repository, ".github", "workflows", "base.yml"), "name: changed\n");

		expect(checkCiDrift({ cwd: repository }).stderr).toBe(
			driftOutput(
				" M .github/workflows/base.yml",
				"?? .github/workflows/a.yml",
				"?? .github/workflows/z.yml",
			),
		);
	});

	test("ignores changes outside the generated workflow directory", () => {
		const repository = createRepository();
		writeFileSync(join(repository, "outside.txt"), "changed outside\n");

		expect(checkCiDrift({ cwd: join(repository, ".github") }).exitCode).toBe(0);
	});

	test("fails closed with Git diagnostics outside a repository", () => {
		const directory = createTempDirectory("not-git");
		const result = checkCiDrift({ cwd: directory });

		expect(result.exitCode).toBe(1);
		expect(result.stderr).toContain("failed while resolving the repository root");
		expect(result.stderr).toContain("git rev-parse --show-toplevel");
		expect(result.stderr).toContain("not a git repository");
	});

	test("fails closed when Git cannot be spawned", () => {
		const directory = createTempDirectory("missing-git");
		const result = checkCiDrift({
			cwd: directory,
			gitCommand: "waddle-test-missing-git-command",
		});

		expect(result.exitCode).toBe(1);
		expect(result.stderr).toContain("failed while resolving the repository root");
		expect(result.stderr).toContain("waddle-test-missing-git-command rev-parse --show-toplevel");
		expect(result.stderr).toMatch(/Executable not found|ENOENT/u);
	});
});

describe("generated server workflow contract", () => {
	for (const workflow of [
		"waddle-server-default.yml",
		"waddle-server-pullrequest.yml",
	]) {
		test(`${workflow} installs and syncs cuenv before the non-recursive drift task`, () => {
			const repositoryRoot = resolve(import.meta.dir, "..", "..");
			const contents = readFileSync(
				join(repositoryRoot, ".github", "workflows", workflow),
				"utf8",
			);
			const testJob = workflowJob(contents, "testCiDrift");
			const checkJob = workflowJob(contents, "checkCiDrift");
			const checkSetup = checkJob.indexOf("- name: Setup cuenv (release)");
			const checkStep = checkJob.indexOf("- name: checkCiDrift");

			expect(testJob).toContain("name: testCiDrift");
			expect(testJob).toContain("run: cuenv task testCiDrift --skip-dependencies");
			expect(checkJob).toContain("name: checkCiDrift");
			expect(checkJob).toContain("needs:\n    - testCiDrift");
			expect(checkSetup).toBeGreaterThanOrEqual(0);
			expect(checkStep).toBeGreaterThan(checkSetup);
			expect(checkJob).toContain("/usr/local/bin/cuenv sync -A");
			expect(checkJob).toContain("run: cuenv task checkCiDrift --skip-dependencies");
		});
	}
});
