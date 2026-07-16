/**
 * CI build-and-publish script for @waddle/xmpp-client-wasm.
 *
 * Runs in the CI `publishWasm` pipeline on every merge to main.
 * Requires: wasm-pack, wasm32-unknown-unknown Rust target, GITHUB_TOKEN env var.
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
export const GITHUB_PACKAGES_REGISTRY = "https://npm.pkg.github.com";
const PUBLISH_AUTH_ENVIRONMENT_NAMES = new Set([
	"bun_config_token",
	"github_token",
	"node_auth_token",
	"npm_auth_token",
	"npm_config_registry",
	"npm_config_token",
	"npm_token",
]);

function publisherEnvironment(environment) {
	const sanitized = Object.fromEntries(
		Object.entries(environment).filter(
			([name]) => !PUBLISH_AUTH_ENVIRONMENT_NAMES.has(name.toLowerCase()),
		),
	);
	sanitized.NPM_CONFIG_TOKEN = environment.GITHUB_TOKEN;
	return sanitized;
}

export function publishPackage(outDir, environment, execute = execFileSync) {
	if (!environment.GITHUB_TOKEN) {
		throw new Error("GITHUB_TOKEN is required to publish to GitHub Packages.");
	}
	execute(
		"bun",
		[
			"publish",
			"--access",
			"public",
			"--registry",
			GITHUB_PACKAGES_REGISTRY,
			"--tolerate-republish",
		],
		{
			cwd: outDir,
			stdio: "inherit",
			env: publisherEnvironment(environment),
		},
	);
}

export function buildAndPublishWasm({
	repoRoot = defaultRepoRoot,
	buildScriptPath = defaultBuildScriptPath,
	environment = process.env,
	loadIdentity = canonicalWasmBuildIdentity,
	createBuildPaths = createIsolatedWasmBuildPaths,
	runBuild = runPinnedWasmBuild,
	compareArtifacts = assertWasmArtifactSetsEqual,
	publishExecute = execFileSync,
	publish = (outDir, publishEnvironment) =>
		publishPackage(outDir, publishEnvironment, publishExecute),
	createRunRoot = () =>
		mkdtempSync(resolve(tmpdir(), "waddle-xmpp-wasm-publish-")),
	removeRunRoot = (path) => rmSync(path, { recursive: true, force: true }),
	log = console.log,
} = {}) {
	if (!environment.GITHUB_TOKEN) {
		throw new Error("GITHUB_TOKEN is required to publish to GitHub Packages.");
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

		compareArtifacts(
			firstBuild.outDir,
			secondBuild.outDir,
			contract.executor.artifactCount,
		);
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
