import { Buffer } from "node:buffer";
import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

export const EXPECTED_RUNTIME_KEYS = Object.freeze([
	"apps/android",
	"chat",
	"colony",
	"infrastructure/waddle.cloud",
	"server",
	"website",
]);

export const EXPECTED_PROJECT_WORKFLOWS = Object.freeze({
	"waddle-android": Object.freeze([
		"waddle-android-default.yml",
		"waddle-android-devicetests.yml",
		"waddle-android-pullrequest.yml",
	]),
	"waddle-chat": Object.freeze([
		"waddle-chat-default.yml",
		"waddle-chat-publishwasm.yml",
		"waddle-chat-pullrequest.yml",
	]),
	"waddle-cloud": Object.freeze(["waddle-cloud-default.yml"]),
	"waddle-colony": Object.freeze([
		"waddle-colony-default.yml",
		"waddle-colony-pullrequest.yml",
	]),
	"waddle-server": Object.freeze([
		"waddle-server-default.yml",
		"waddle-server-publish-tags-to-flakehub.yml",
		"waddle-server-pullrequest.yml",
		"waddle-server-xmppcompliance.yml",
	]),
	"waddle-website": Object.freeze([
		"waddle-website-default.yml",
		"waddle-website-pullrequest.yml",
	]),
});

const EXPECTED_CHAT_RUNTIME_FIELDS = Object.freeze([
	"digest",
	"flake",
	"lockfile",
	"type",
]);
const EXCLUDED_ENV_PREFIXES = Object.freeze([
	".agents/",
	".cuenv/",
	".git/",
	"cue.mod/pkg/",
	"node_modules/",
	"xeps/",
]);

function bytewiseCompare(left, right) {
	return left < right ? -1 : left > right ? 1 : 0;
}

function sorted(values) {
	return [...values].sort(bytewiseCompare);
}

function duplicateValues(values) {
	const counts = new Map();
	for (const value of values) counts.set(value, (counts.get(value) ?? 0) + 1);
	return sorted(
		[...counts.entries()]
			.filter(([, count]) => count > 1)
			.map(([value]) => value),
	);
}

function renderValues(values) {
	return values.length === 0 ? "(none)" : values.join(", ");
}

export function assertExactSet(label, actualValues, expectedValues) {
	const actual = sorted(new Set(actualValues));
	const expected = sorted(new Set(expectedValues));
	const actualSet = new Set(actual);
	const expectedSet = new Set(expected);
	const missing = expected.filter((value) => !actualSet.has(value));
	const extra = actual.filter((value) => !expectedSet.has(value));
	const duplicates = duplicateValues(actualValues);

	if (missing.length === 0 && extra.length === 0 && duplicates.length === 0) {
		return;
	}

	throw new Error(
		`${label} mismatch:\n` +
			`  expected: ${renderValues(expected)}\n` +
			`  actual: ${renderValues(actual)}\n` +
			`  missing: ${renderValues(missing)}\n` +
			`  extra: ${renderValues(extra)}\n` +
			`  duplicates: ${renderValues(duplicates)}`,
	);
}

function record(value, label) {
	if (value === null || typeof value !== "object" || Array.isArray(value)) {
		throw new Error(`${label} must be a TOML table`);
	}
	return value;
}

export function assertLockContract(contents) {
	let parsed;
	try {
		parsed = Bun.TOML.parse(contents);
	} catch (error) {
		throw new Error(
			`cuenv.lock is not valid TOML: ${error instanceof Error ? error.message : String(error)}`,
		);
	}

	const runtimes = record(
		record(parsed, "cuenv.lock").runtimes,
		"cuenv.lock runtimes",
	);
	assertExactSet(
		"cuenv.lock runtime keys",
		Object.keys(runtimes),
		EXPECTED_RUNTIME_KEYS,
	);

	const chat = record(runtimes.chat, "cuenv.lock runtimes.chat");
	assertExactSet(
		"cuenv.lock runtimes.chat fields",
		Object.keys(chat),
		EXPECTED_CHAT_RUNTIME_FIELDS,
	);

	const invalid = [];
	if (chat.type !== "nix")
		invalid.push(`type expected "nix", got ${JSON.stringify(chat.type)}`);
	if (chat.flake !== "..")
		invalid.push(`flake expected "..", got ${JSON.stringify(chat.flake)}`);
	if (chat.lockfile !== "chat/../flake.lock") {
		invalid.push(
			`lockfile expected "chat/../flake.lock", got ${JSON.stringify(chat.lockfile)}`,
		);
	}
	if (
		typeof chat.digest !== "string" ||
		!/^sha256:[0-9a-f]{64}$/u.test(chat.digest)
	) {
		invalid.push(
			`digest expected sha256:<64 lowercase hex>, got ${JSON.stringify(chat.digest)}`,
		);
	}
	if (invalid.length > 0) {
		throw new Error(
			`cuenv.lock runtimes.chat has invalid Nix fields:\n- ${invalid.join("\n- ")}`,
		);
	}
}

export function lockLineDiff(expected, actual, limit = 8) {
	const expectedLines = expected.split(/\r?\n/u);
	const actualLines = actual.split(/\r?\n/u);
	const differences = [];
	const count = Math.max(expectedLines.length, actualLines.length);
	for (let index = 0; index < count && differences.length < limit; index += 1) {
		if (expectedLines[index] === actualLines[index]) continue;
		differences.push(
			`@@ line ${index + 1} @@\n` +
				`- ${expectedLines[index] ?? "<missing>"}\n` +
				`+ ${actualLines[index] ?? "<missing>"}`,
		);
	}
	return differences.join("\n");
}

export function assertByteStable(label, actual, expected) {
	if (Buffer.from(actual, "utf8").equals(Buffer.from(expected, "utf8"))) return;
	throw new Error(
		`${label} is not byte-stable:\n${lockLineDiff(expected, actual)}`,
	);
}

export function discoverWorkspaceProjects(repoRoot) {
	const projects = [];
	const envFiles = new Bun.Glob("**/env.cue").scanSync({
		cwd: repoRoot,
		onlyFiles: true,
	});
	for (const relativePath of envFiles) {
		if (EXCLUDED_ENV_PREFIXES.some((prefix) => relativePath.startsWith(prefix)))
			continue;
		const source = readFileSync(join(repoRoot, relativePath), "utf8");
		const projectIndex = source.indexOf("schema.#Project");
		if (projectIndex < 0) continue;
		const name = source.slice(projectIndex).match(/\bname:\s*"([^"]+)"/u)?.[1];
		if (!name) {
			throw new Error(
				`workspace project ${relativePath} has no literal project name`,
			);
		}
		projects.push(name);
	}
	return projects;
}

export function assertProjectWorkflowContract(repoRoot) {
	const projectNames = discoverWorkspaceProjects(repoRoot);
	const expectedProjects = Object.keys(EXPECTED_PROJECT_WORKFLOWS);
	assertExactSet("workspace project names", projectNames, expectedProjects);

	const workflowDirectory = join(repoRoot, ".github", "workflows");
	const workflowNames = readdirSync(workflowDirectory, { withFileTypes: true })
		.filter((entry) => entry.isFile() && /\.ya?ml$/u.test(entry.name))
		.map((entry) => entry.name);
	for (const project of expectedProjects) {
		const actual = workflowNames.filter((name) =>
			name.startsWith(`${project}-`),
		);
		assertExactSet(
			`generated workflows for ${project}`,
			actual,
			EXPECTED_PROJECT_WORKFLOWS[project],
		);
	}
}

function runCommand(command, args, cwd) {
	const result = spawnSync(command, args, {
		cwd,
		encoding: "utf8",
		env: { ...process.env, LC_ALL: "C" },
	});
	if (result.error || result.status !== 0) {
		const status =
			result.status == null ? "not started" : `exit ${String(result.status)}`;
		const detail =
			result.error?.message ?? result.stderr?.trim() ?? "no diagnostic output";
		throw new Error(
			`${command} ${args.join(" ")} failed (${status}): ${detail}`,
		);
	}
	return { stdout: result.stdout ?? "", stderr: result.stderr ?? "" };
}

export function checkRootSyncDrift({
	repoRoot = resolve(import.meta.dir, "..", ".."),
	commandRunner = runCommand,
} = {}) {
	const lockPath = join(repoRoot, "cuenv.lock");
	let canonicalLock;
	let syncStarted = false;
	try {
		canonicalLock = commandRunner(
			"git",
			["show", "HEAD:cuenv.lock"],
			repoRoot,
		).stdout;
		if (canonicalLock.length === 0)
			throw new Error("git show HEAD:cuenv.lock returned no bytes");
		const canonicalGitignore = commandRunner(
			"git",
			["show", "HEAD:.gitignore"],
			repoRoot,
		).stdout;

		assertLockContract(canonicalLock);
		assertProjectWorkflowContract(repoRoot);
		const workingLock = readFileSync(lockPath, "utf8");
		assertLockContract(workingLock);
		assertByteStable(
			"working cuenv.lock before the guard (compared with git show HEAD:cuenv.lock)",
			workingLock,
			canonicalLock,
		);
		assertByteStable(
			"working .gitignore before the guard (compared with git show HEAD:.gitignore)",
			readFileSync(join(repoRoot, ".gitignore"), "utf8"),
			canonicalGitignore,
		);

		syncStarted = true;
		let firstSynchronizedLock;
		for (let attempt = 1; attempt <= 2; attempt += 1) {
			commandRunner("cuenv", ["sync", "lock", "-A"], repoRoot);
			const synchronizedLock = readFileSync(lockPath, "utf8");
			assertLockContract(synchronizedLock);
			assertByteStable(
				`cuenv.lock after workspace synchronization ${attempt} (compared with git show HEAD:cuenv.lock)`,
				synchronizedLock,
				canonicalLock,
			);
			if (firstSynchronizedLock !== undefined) {
				assertByteStable(
					"cuenv.lock across two workspace synchronizations",
					synchronizedLock,
					firstSynchronizedLock,
				);
			}
			firstSynchronizedLock = synchronizedLock;
		}

		commandRunner("cuenv", ["sync", "--check", "-p", repoRoot], repoRoot);
		assertByteStable(
			"cuenv.lock after root rules/VCS synchronization check",
			readFileSync(lockPath, "utf8"),
			canonicalLock,
		);
		assertByteStable(
			".gitignore after root rules/VCS synchronization check",
			readFileSync(join(repoRoot, ".gitignore"), "utf8"),
			canonicalGitignore,
		);

		const workflowCount = Object.values(EXPECTED_PROJECT_WORKFLOWS).reduce(
			(total, workflows) => total + workflows.length,
			0,
		);
		return {
			exitCode: 0,
			stdout:
				`Root sync guard verified ${EXPECTED_RUNTIME_KEYS.length} runtimes, ` +
				`${Object.keys(EXPECTED_PROJECT_WORKFLOWS).length} projects, ${workflowCount} workflows, ` +
				"and two byte-stable workspace lock synchronizations.\n",
			stderr: "",
		};
	} catch (error) {
		let restoreFailure = "";
		if (syncStarted && canonicalLock !== undefined) {
			try {
				const current = readFileSync(lockPath, "utf8");
				if (!Buffer.from(current).equals(Buffer.from(canonicalLock))) {
					writeFileSync(lockPath, canonicalLock);
				}
			} catch (restoreError) {
				restoreFailure = `\nFailed to restore committed cuenv.lock bytes: ${
					restoreError instanceof Error
						? restoreError.message
						: String(restoreError)
				}`;
			}
		}
		return {
			exitCode: 1,
			stdout: "",
			stderr: `Root sync guard failed:\n${
				error instanceof Error ? error.message : String(error)
			}${restoreFailure}\n`,
		};
	}
}

if (import.meta.main) {
	const result = checkRootSyncDrift();
	if (result.stdout) process.stdout.write(result.stdout);
	if (result.stderr) process.stderr.write(result.stderr);
	process.exitCode = result.exitCode;
}
