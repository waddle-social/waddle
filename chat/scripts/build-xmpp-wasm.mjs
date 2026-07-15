import { execFileSync } from "node:child_process";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	realpathSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
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
const committedOutDir = resolve(
	repoRoot,
	"server/wasm-pkg/waddle-xmpp-client-wasm",
);
const nodeModulesDir = resolve(
	scriptDir,
	"..",
	"node_modules/@waddle/xmpp-client-wasm",
);
const checkOnly = process.argv.slice(2).includes("--check");
const outDir = checkOnly
	? mkdtempSync(resolve(tmpdir(), "waddle-xmpp-wasm-check-"))
	: committedOutDir;
const trackedGeneratedFiles = [
	"package.json",
	"waddle_xmpp_client_wasm.d.ts",
	"waddle_xmpp_client_wasm.js",
];
assertHermeticWasmBuildEnvironment(process.env);
resolvePinnedNixToolchain();
const {
	buildId,
	contract,
	entries: buildInputs,
} = canonicalWasmBuildIdentity(repoRoot);
const crateDir = resolve(repoRoot, ...contract.crate.split("/"));

if (checkOnly) {
	process.on("exit", () => rmSync(outDir, { recursive: true, force: true }));
}

function newestBuildInputMtime() {
	return Math.max(
		...buildInputs.map(
			({ path }) => statSync(resolve(repoRoot, ...path.split("/"))).mtimeMs,
		),
	);
}

if (!checkOnly && process.env.REBUILD_WASM !== "1") {
	// If _bg.js doesn't exist in node_modules, the package is incomplete (fresh checkout or
	// bun install after the artifact was removed). Also rebuild when the Rust sources are
	// newer than the installed artifact; otherwise an existing node_modules can keep exposing
	// stale wasm-bindgen methods after a branch checkout.
	const bgJsPath = resolve(nodeModulesDir, "waddle_xmpp_client_wasm_bg.js");
	const realBgJsPath = existsSync(nodeModulesDir)
		? resolve(realpathSync(nodeModulesDir), "waddle_xmpp_client_wasm_bg.js")
		: bgJsPath;
	if (!existsSync(realBgJsPath)) {
		console.log(
			"[wasm] WASM artifacts missing — auto-rebuilding from Rust source...",
		);
		console.log(
			"[wasm] (Set REBUILD_WASM=1 explicitly to force a rebuild when artifacts exist.)",
		);
		// Fall through to the rebuild path below.
	} else if (newestBuildInputMtime() > statSync(realBgJsPath).mtimeMs) {
		console.log(
			"[wasm] Rust wasm sources changed — rebuilding @waddle/xmpp-client-wasm...",
		);
	} else {
		// Artifacts present; rely on the local build or published registry version.
		console.log(
			"[wasm] WASM artifacts found — skipping rebuild. Set REBUILD_WASM=1 to force recompile.",
		);
		process.exit(0);
	}
} else if (checkOnly) {
	console.log(
		"[wasm] Checking committed WASM bindings against a forced temporary rebuild...",
	);
} else {
	console.log(
		"[wasm] REBUILD_WASM=1 — building @waddle/xmpp-client-wasm from Rust source...",
	);
}

execFileSync(contract.wasmPack.command, wasmPackBuildArgs(contract, outDir), {
	cwd: crateDir,
	stdio: "inherit",
});

// Replace the wasm-pack bundler entry point with a Vite-compatible one that
// passes the correct import object so WebAssembly.instantiate() receives the
// JS glue functions the WASM binary imports from "./waddle_xmpp_client_wasm_bg.js".
//
// We import the WASM binary as a URL (?url) and instantiate it manually so that
// in dev mode we can set cache: "no-store" on the fetch.  This prevents the
// browser from serving a stale compiled WebAssembly module after a rebuild,
// which would cause "wasm.__wasm_bindgen_func_elem_N is not a function" errors
// when the .wasm binary and _bg.js glue are from different builds.
// In production the ?url resolves to a content-hashed filename so the browser
// cache is correctly busted without needing no-store.
//
// The full cache-bust ID comes only from the versioned canonical input
// manifest, never from host-sensitive compiled WASM or glue bytes.
finalizeWasmPackage(outDir, buildId);

if (checkOnly) {
	const drifted = trackedGeneratedFiles.filter((file) => {
		const committed = resolve(committedOutDir, file);
		const rebuilt = resolve(outDir, file);
		return (
			!existsSync(committed) ||
			!existsSync(rebuilt) ||
			!readFileSync(committed).equals(readFileSync(rebuilt))
		);
	});
	if (drifted.length > 0) {
		throw new Error(
			`committed WASM bindings are stale: ${drifted.join(", ")}; run REBUILD_WASM=1 bun run wasm:build`,
		);
	}
	console.log("[wasm] Committed WASM bindings match the forced rebuild.");
} else {
	// Copy compiled artifacts into node_modules so the local build uses this version.
	// We write each file individually (writeFileSync, not cpSync) so that bun's
	// hardlinked package cache is updated in-place: bun hardlinks the same inode
	// for both the file: path and its internal .bun/ cache directory. cpSync would
	// create new inodes (breaking hardlinks and leaving the bun cache stale), while
	// writeFileSync writes through the existing inode so every hardlinked copy sees
	// the new content immediately.
	console.log("[wasm] Installing local build into node_modules...");
	const realNodeModulesDir = existsSync(nodeModulesDir)
		? realpathSync(nodeModulesDir)
		: nodeModulesDir;
	mkdirSync(realNodeModulesDir, { recursive: true });
	function copyDirInPlace(src, dest) {
		mkdirSync(dest, { recursive: true });
		for (const entry of readdirSync(src)) {
			const srcPath = resolve(src, entry);
			const destPath = resolve(dest, entry);
			if (statSync(srcPath).isDirectory()) {
				copyDirInPlace(srcPath, destPath);
			} else {
				writeFileSync(destPath, readFileSync(srcPath));
			}
		}
	}
	copyDirInPlace(outDir, realNodeModulesDir);

	// Clear Vite's module transform cache so it doesn't serve stale _bg.js from a previous build.
	// Without this, a mismatch between the cached glue JS and the newly compiled .wasm causes
	// runtime errors like "wasm.__wasm_bindgen_func_elem_N is not a function".
	const viteCacheDir = resolve(scriptDir, "..", "node_modules", ".vite");
	if (existsSync(viteCacheDir)) {
		rmSync(viteCacheDir, { recursive: true, force: true });
		console.log("[wasm] Cleared Vite module cache.");
	}

	console.log("[wasm] Done. Local build installed.");
	console.log(
		"[wasm] ⚠️  Next `bun install` will revert to the published registry version.",
	);
}
