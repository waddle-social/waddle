/**
 * CI build-and-publish script for @waddle/xmpp-client-wasm.
 *
 * Runs in the CI `publishWasm` pipeline on every merge to main.
 * Requires: wasm-pack, wasm32-unknown-unknown Rust target, NODE_AUTH_TOKEN env var.
 *
 * Usage: bun run scripts/build-and-publish-wasm.mjs
 */
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
	assertHermeticWasmBuildEnvironment,
	canonicalWasmBuildIdentity,
	resolvePinnedNixToolchain,
	wasmPackBuildArgs,
} from "./wasm-build-inputs.mjs";
import { finalizeWasmPackage } from "./wasm-package-bindings.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..", "..");
const outDir = resolve(repoRoot, "server/wasm-pkg/waddle-xmpp-client-wasm");

if (!process.env.NODE_AUTH_TOKEN) {
	console.error(
		"[wasm] NODE_AUTH_TOKEN is required to publish to GitHub Packages.",
	);
	process.exit(1);
}
assertHermeticWasmBuildEnvironment(process.env);
resolvePinnedNixToolchain();
const { buildId, contract } = canonicalWasmBuildIdentity(repoRoot);
const crateDir = resolve(repoRoot, ...contract.crate.split("/"));

// Build
console.log("[wasm] Building @waddle/xmpp-client-wasm from Rust source...");
execFileSync(contract.wasmPack.command, wasmPackBuildArgs(contract, outDir), {
	cwd: crateDir,
	stdio: "inherit",
});

finalizeWasmPackage(outDir, buildId);

// Publish to GitHub Packages.
console.log("[wasm] Publishing @waddle/xmpp-client-wasm to GitHub Packages...");
execFileSync("bun", ["publish", "--access", "public"], {
	cwd: outDir,
	stdio: "inherit",
	env: { ...process.env },
});

console.log("[wasm] Published successfully.");
