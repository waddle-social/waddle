import { afterEach, describe, expect, test } from "bun:test";
import {
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
	EXPECTED_PROJECT_WORKFLOWS,
	assertExactSet,
	assertLockContract,
	checkRootSyncDrift,
} from "./check-root-sync-drift.mjs";

const tempDirectories: string[] = [];
const canonicalGitignore = ".cuenv\nnode_modules\n";
const canonicalLock = `version = 4

[runtimes."apps/android"]
type = "nix"

[runtimes.chat]
type = "nix"
flake = ".."
digest = "sha256:22c72a3faea22e798923cadadad88c625122228e05204cc052fc74d5f2d8d883"
lockfile = "chat/../flake.lock"

[runtimes.colony]
type = "nix"

[runtimes."infrastructure/waddle.cloud"]
type = "nix"

[runtimes.server]
type = "nix"

[runtimes.website]
type = "nix"
`;
const projectPaths = Object.freeze({
	"waddle-android": "apps/android/env.cue",
	"waddle-chat": "chat/env.cue",
	"waddle-cloud": "infrastructure/waddle.cloud/env.cue",
	"waddle-colony": "colony/env.cue",
	"waddle-server": "server/env.cue",
	"waddle-website": "website/env.cue",
});

function createTempDirectory(): string {
	const directory = mkdtempSync(join(tmpdir(), "waddle-root-sync-"));
	tempDirectories.push(directory);
	return directory;
}

function write(
	repository: string,
	relativePath: string,
	contents: string,
): void {
	const path = join(repository, relativePath);
	mkdirSync(resolve(path, ".."), { recursive: true });
	writeFileSync(path, contents);
}

function createRepository(): string {
	const repository = createTempDirectory();
	write(repository, "cuenv.lock", canonicalLock);
	write(repository, ".gitignore", canonicalGitignore);
	for (const [name, relativePath] of Object.entries(projectPaths)) {
		write(
			repository,
			relativePath,
			`package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {name: "${name}"}\n`,
		);
	}
	for (const workflows of Object.values(EXPECTED_PROJECT_WORKFLOWS)) {
		for (const workflow of workflows) {
			write(repository, `.github/workflows/${workflow}`, `name: ${workflow}\n`);
		}
	}
	return repository;
}

function withoutChatRuntime(contents: string): string {
	return contents.replace(
		/\n\[runtimes\.chat\][\s\S]*?(?=\n\[runtimes\.colony\])/u,
		"",
	);
}

function commandHarness({
	onLockSync,
	onRootSync,
}: {
	onLockSync?: (attempt: number, repository: string) => void;
	onRootSync?: (repository: string) => void;
} = {}) {
	const calls = { lockSync: 0, rootSync: 0 };
	return {
		calls,
		commandRunner(command: string, args: string[], repository: string) {
			if (command === "git" && args.join(" ") === "show HEAD:cuenv.lock") {
				return { stdout: canonicalLock, stderr: "" };
			}
			if (command === "git" && args.join(" ") === "show HEAD:.gitignore") {
				return { stdout: canonicalGitignore, stderr: "" };
			}
			if (command === "cuenv" && args.join(" ") === "sync lock -A") {
				calls.lockSync += 1;
				onLockSync?.(calls.lockSync, repository);
				return { stdout: "Lockfile is up to date.\n", stderr: "" };
			}
			if (
				command === "cuenv" &&
				args[0] === "sync" &&
				args[1] === "--check" &&
				args[2] === "-p"
			) {
				calls.rootSync += 1;
				onRootSync?.(repository);
				return { stdout: "root sync clean\n", stderr: "" };
			}
			throw new Error(`unexpected command: ${command} ${args.join(" ")}`);
		},
	};
}

afterEach(() => {
	for (const directory of tempDirectories.splice(0)) {
		rmSync(directory, { recursive: true, force: true });
	}
});

describe("root sync fail-closed contract", () => {
	test("accepts exactly six runtimes, six projects, fifteen workflows, and two stable lock syncs", () => {
		const repository = createRepository();
		const harness = commandHarness();

		const result = checkRootSyncDrift({
			repoRoot: repository,
			commandRunner: harness.commandRunner,
		});

		expect(result).toEqual({
			exitCode: 0,
			stdout:
				"Root sync guard verified 6 runtimes, 6 projects, 15 workflows, and two byte-stable workspace lock synchronizations.\n",
			stderr: "",
		});
		expect(harness.calls).toEqual({ lockSync: 2, rootSync: 1 });
	});

	test("fails before synchronization when setup already removed the chat runtime", () => {
		const repository = createRepository();
		write(repository, "cuenv.lock", withoutChatRuntime(canonicalLock));
		const harness = commandHarness();

		const result = checkRootSyncDrift({
			repoRoot: repository,
			commandRunner: harness.commandRunner,
		});

		expect(result.exitCode).toBe(1);
		expect(result.stderr).toContain("cuenv.lock runtime keys mismatch");
		expect(result.stderr).toContain("missing: chat");
		expect(result.stderr).toContain(
			"actual: apps/android, colony, infrastructure/waddle.cloud, server, website",
		);
		expect(harness.calls).toEqual({ lockSync: 0, rootSync: 0 });
	});

	test("reports extra and duplicate set members exactly", () => {
		expect(() =>
			assertExactSet("projects", ["chat", "chat", "extra"], ["chat", "server"]),
		).toThrow(
			"projects mismatch:\n" +
				"  expected: chat, server\n" +
				"  actual: chat, extra\n" +
				"  missing: server\n" +
				"  extra: extra\n" +
				"  duplicates: chat",
		);
	});

	test("rejects an extra runtime and invalid chat Nix fields", () => {
		expect(() =>
			assertLockContract(
				`${canonicalLock}\n[runtimes.unexpected]\ntype = "nix"\n`,
			),
		).toThrow("extra: unexpected");
		expect(() =>
			assertLockContract(
				canonicalLock.replace('flake = ".."', 'flake = "../wrong"'),
			),
		).toThrow('flake expected "..", got "../wrong"');
	});

	test("fails loudly for a missing project and an extra project", () => {
		const missingRepository = createRepository();
		unlinkSync(join(missingRepository, projectPaths["waddle-website"]));
		const missing = checkRootSyncDrift({
			repoRoot: missingRepository,
			commandRunner: commandHarness().commandRunner,
		});
		expect(missing.stderr).toContain("workspace project names mismatch");
		expect(missing.stderr).toContain("missing: waddle-website");

		const extraRepository = createRepository();
		write(
			extraRepository,
			"extra/env.cue",
			'package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {name: "waddle-extra"}\n',
		);
		const extra = checkRootSyncDrift({
			repoRoot: extraRepository,
			commandRunner: commandHarness().commandRunner,
		});
		expect(extra.stderr).toContain("extra: waddle-extra");
	});

	test("fails loudly for missing and extra chat workflows", () => {
		const missingRepository = createRepository();
		unlinkSync(
			join(missingRepository, ".github/workflows/waddle-chat-publishwasm.yml"),
		);
		const missing = checkRootSyncDrift({
			repoRoot: missingRepository,
			commandRunner: commandHarness().commandRunner,
		});
		expect(missing.stderr).toContain(
			"generated workflows for waddle-chat mismatch",
		);
		expect(missing.stderr).toContain("missing: waddle-chat-publishwasm.yml");

		const extraRepository = createRepository();
		write(
			extraRepository,
			".github/workflows/waddle-chat-unexpected.yml",
			"name: unexpected\n",
		);
		const extra = checkRootSyncDrift({
			repoRoot: extraRepository,
			commandRunner: commandHarness().commandRunner,
		});
		expect(extra.stderr).toContain("extra: waddle-chat-unexpected.yml");
	});

	test("rejects a partial first sync and restores committed lock bytes", () => {
		const repository = createRepository();
		const harness = commandHarness({
			onLockSync: (_attempt, root) => {
				write(root, "cuenv.lock", withoutChatRuntime(canonicalLock));
			},
		});

		const result = checkRootSyncDrift({
			repoRoot: repository,
			commandRunner: harness.commandRunner,
		});

		expect(result.stderr).toContain("missing: chat");
		expect(harness.calls.lockSync).toBe(1);
		expect(readFileSync(join(repository, "cuenv.lock"), "utf8")).toBe(
			canonicalLock,
		);
	});

	test("rejects a byte-unstable second sync and restores committed lock bytes", () => {
		const repository = createRepository();
		const harness = commandHarness({
			onLockSync: (attempt, root) => {
				if (attempt === 2) write(root, "cuenv.lock", `${canonicalLock}\n`);
			},
		});

		const result = checkRootSyncDrift({
			repoRoot: repository,
			commandRunner: harness.commandRunner,
		});

		expect(result.stderr).toContain(
			"cuenv.lock after workspace synchronization 2 (compared with git show HEAD:cuenv.lock) is not byte-stable",
		);
		expect(result.stderr).toContain("@@ line");
		expect(harness.calls.lockSync).toBe(2);
		expect(readFileSync(join(repository, "cuenv.lock"), "utf8")).toBe(
			canonicalLock,
		);
	});

	test("preserves the existing root rules and VCS check", () => {
		const repository = createRepository();
		const harness = commandHarness({
			onRootSync: (root) =>
				write(root, ".gitignore", `${canonicalGitignore}drift\n`),
		});

		const result = checkRootSyncDrift({
			repoRoot: repository,
			commandRunner: harness.commandRunner,
		});

		expect(result.stderr).toContain(
			".gitignore after root rules/VCS synchronization check is not byte-stable",
		);
		expect(harness.calls.rootSync).toBe(1);
	});
});

describe("root sync task wiring", () => {
	test("runs tests and the guard sequentially inside the existing task node", () => {
		const repositoryRoot = resolve(import.meta.dir, "..", "..");
		const source = readFileSync(
			join(repositoryRoot, "server", "env.cue"),
			"utf8",
		);
		const start = source.indexOf("\t\tcheckRootSyncDrift: schema.#Task");
		const end = source.indexOf(
			"\n\t\tcheckSwitchableAlternativeProgram:",
			start,
		);
		const task = source.slice(start, end);

		expect(start).toBeGreaterThanOrEqual(0);
		expect(end).toBeGreaterThan(start);
		expect(task).toContain("set -euo pipefail");
		expect(
			task.indexOf("bun test scripts/check-root-sync-drift.test.ts"),
		).toBeLessThan(task.indexOf("bun scripts/check-root-sync-drift.mjs"));
		expect(source.match(/checkRootSyncDrift: schema\.#Task/gu)).toHaveLength(1);
	});
});
