/**
 * CI build-and-publish script for @waddle/xmpp-client-wasm.
 *
 * Runs in the CI `publishWasm` pipeline on every merge to main.
 * Requires: wasm-pack, wasm32-unknown-unknown Rust target, NODE_AUTH_TOKEN env var.
 *
 * Usage: bun run scripts/build-and-publish-wasm.mjs
 */
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { canonicalWasmBuildIdentity } from "./wasm-build-inputs.mjs";
import {
	assertWasmArtifactSetsEqual,
	createIsolatedWasmBuildPaths,
	runPinnedWasmBuild,
} from "./wasm-build-executor.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const defaultRepoRoot = resolve(scriptDir, "..", "..");
const defaultBuildScriptPath = resolve(scriptDir, "build-xmpp-wasm.mjs");

function publishPackage(outDir, environment) {
	execFileSync("bun", ["publish", "--access", "public"], {
		cwd: outDir,
		stdio: "inherit",
		env: { ...environment },
	});
}

export function buildAndPublishWasm({
	repoRoot = defaultRepoRoot,
	buildScriptPath = defaultBuildScriptPath,
	environment = process.env,
	loadIdentity = canonicalWasmBuildIdentity,
	createBuildPaths = createIsolatedWasmBuildPaths,
	runBuild = runPinnedWasmBuild,
	compareArtifacts = assertWasmArtifactSetsEqual,
	publish = publishPackage,
	createRunRoot = () =>
		mkdtempSync(resolve(tmpdir(), "waddle-xmpp-wasm-publish-")),
	removeRunRoot = (path) => rmSync(path, { recursive: true, force: true }),
	log = console.log,
} = {}) {
	if (!environment.NODE_AUTH_TOKEN) {
		throw new Error(
			"NODE_AUTH_TOKEN is required to publish to GitHub Packages.",
		);
	}
	const { buildId, contract } = loadIdentity(repoRoot);
	const runRoot = createRunRoot();
	try {
		const firstBuild = createBuildPaths(runRoot, "first");
		const secondBuild = createBuildPaths(runRoot, "second");
		log(
			"[wasm] Building two isolated @waddle/xmpp-client-wasm packages through the pinned repository flake...",
		);
		for (const build of [firstBuild, secondBuild]) {
			runBuild({
				repoRoot,
				scriptPath: buildScriptPath,
				outDir: build.outDir,
				paths: build,
				contract,
				buildId,
				environment,
			});
		}

		compareArtifacts(firstBuild.outDir, secondBuild.outDir);
		log(
			"[wasm] Two isolated six-artifact builds match; publishing the attested first package...",
		);
		publish(firstBuild.outDir, environment);
		log("[wasm] Published successfully.");
	} finally {
		removeRunRoot(runRoot);
	}
}

if (import.meta.main) {
	buildAndPublishWasm();
}
