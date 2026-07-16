import { Buffer } from "node:buffer";
import { spawnSync } from "node:child_process";

const workflowPathspec = ".github/workflows";

const remediation = `Regenerate the workflows and commit every generated change:
  cuenv sync ci -A
Commit all generated workflow changes under .github/workflows.`;

function runGit(gitCommand, args, cwd) {
	return spawnSync(gitCommand, args, {
		cwd,
		encoding: "utf8",
		env: { ...process.env, LC_ALL: "C" },
	});
}

function gitFailure(stage, command, result) {
	const status = result.status == null ? "not started" : String(result.status);
	const outcome = result.signal ? `signal ${result.signal}` : `exit ${status}`;
	const stderr = typeof result.stderr === "string" ? result.stderr.trim() : "";
	const detail = result.error?.message ?? (stderr || "no diagnostic output");
	return {
		exitCode: 1,
		stdout: "",
		stderr: `CI workflow drift check failed while ${stage} (${command}; ${outcome}).\n${detail}\n`,
	};
}

function sortRecords(stdout) {
	return stdout
		.split(/\r?\n/u)
		.filter((record) => record.length > 0)
		.sort((left, right) =>
			Buffer.compare(Buffer.from(left, "utf8"), Buffer.from(right, "utf8")),
		);
}

export function checkCiDrift({ cwd = process.cwd(), gitCommand = "git" } = {}) {
	const rootArgs = ["rev-parse", "--show-toplevel"];
	const rootResult = runGit(gitCommand, rootArgs, cwd);
	if (rootResult.error || rootResult.status !== 0) {
		return gitFailure(
			"resolving the repository root",
			`${gitCommand} ${rootArgs.join(" ")}`,
			rootResult,
		);
	}

	const repoRoot = rootResult.stdout.trim();
	if (repoRoot.length === 0) {
		return {
			exitCode: 1,
			stdout: "",
			stderr:
				"CI workflow drift check failed while resolving the repository root: git returned an empty path.\n",
		};
	}

	const statusArgs = [
		"status",
		"--porcelain=v1",
		"--untracked-files=all",
		"--",
		workflowPathspec,
	];
	const statusResult = runGit(gitCommand, statusArgs, repoRoot);
	if (statusResult.error || statusResult.status !== 0) {
		return gitFailure(
			"reading generated workflow status",
			`${gitCommand} ${statusArgs.join(" ")}`,
			statusResult,
		);
	}

	const records = sortRecords(statusResult.stdout);
	if (records.length === 0) {
		return { exitCode: 0, stdout: "", stderr: "" };
	}

	return {
		exitCode: 1,
		stdout: "",
		stderr: `Generated GitHub workflow drift detected:\n${records.join("\n")}\n\n${remediation}\n`,
	};
}

if (import.meta.main) {
	const result = checkCiDrift();
	if (result.stdout) process.stdout.write(result.stdout);
	if (result.stderr) process.stderr.write(result.stderr);
	process.exitCode = result.exitCode;
}
