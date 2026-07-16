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
	createIsolatedWasmBuildPaths,
	runPinnedWasmBuild,
} from "./wasm-build-executor.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..", "..");
const buildScriptPath = resolve(scriptDir, "build-xmpp-wasm.mjs");

if (!process.env.NODE_AUTH_TOKEN) {
	console.error(
		"[wasm] NODE_AUTH_TOKEN is required to publish to GitHub Packages.",
	);
	process.exit(1);
}
const { contract } = canonicalWasmBuildIdentity(repoRoot);
const runRoot = mkdtempSync(resolve(tmpdir(), "waddle-xmpp-wasm-publish-"));
try {
	const build = createIsolatedWasmBuildPaths(runRoot, "package");
	console.log(
		"[wasm] Building @waddle/xmpp-client-wasm through the pinned repository flake...",
	);
	runPinnedWasmBuild({
		repoRoot,
		scriptPath: buildScriptPath,
		outDir: build.outDir,
		paths: build,
		contract,
	});

	console.log("[wasm] Publishing @waddle/xmpp-client-wasm to GitHub Packages...");
	execFileSync("bun", ["publish", "--access", "public"], {
		cwd: build.outDir,
		stdio: "inherit",
		env: { ...process.env },
	});

	console.log("[wasm] Published successfully.");
} finally {
	rmSync(runRoot, { recursive: true, force: true });
}
