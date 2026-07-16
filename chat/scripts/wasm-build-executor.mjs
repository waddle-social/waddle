import { execFileSync } from "node:child_process";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	realpathSync,
} from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import {
	assertHermeticWasmBuildEnvironment,
	resolvePinnedNixToolchain,
} from "./wasm-build-environment.mjs";

export const PINNED_WASM_EXECUTOR_PROTOCOL =
	"waddle:xmpp-client-wasm:pinned-flake-executor:v1";
const CANONICAL_BUILD_ID = /^[0-9a-f]{64}$/u;
const ENCODED_RUSTFLAG_SEPARATOR = "\u001f";

export const WASM_PACKAGE_ARTIFACTS = Object.freeze([
	"package.json",
	"waddle_xmpp_client_wasm.d.ts",
	"waddle_xmpp_client_wasm.js",
	"waddle_xmpp_client_wasm_bg.js",
	"waddle_xmpp_client_wasm_bg.wasm",
	"waddle_xmpp_client_wasm_bg.wasm.d.ts",
]);

export const TRACKED_WASM_ARTIFACTS = Object.freeze([
	"package.json",
	"waddle_xmpp_client_wasm.d.ts",
	"waddle_xmpp_client_wasm.js",
]);

function assertContained(root, path, description) {
	const relativePath = relative(root, path);
	if (
		relativePath === "" ||
		relativePath === ".." ||
		relativePath.startsWith(`..${sep}`) ||
		isAbsolute(relativePath)
	) {
		throw new Error(
			`${description} must be an isolated child of ${root}: ${path}`,
		);
	}
}

export function createIsolatedWasmBuildPaths(parentDir, label) {
	mkdirSync(parentDir, { recursive: true });
	const root = mkdtempSync(resolve(parentDir, `waddle-wasm-${label}-`));
	const paths = {
		root,
		cargoHome: resolve(root, "cargo-home"),
		cargoTarget: resolve(root, "cargo-target"),
		home: resolve(root, "home"),
		outDir: resolve(root, "package"),
	};
	for (const path of [
		paths.cargoHome,
		paths.cargoTarget,
		paths.home,
		paths.outDir,
	]) {
		mkdirSync(path);
		assertContained(root, path, "canonical WASM build path");
	}
	return paths;
}

function expectedToolchain(environment) {
	return {
		executables: {
			bun: environment.WADDLE_FLAKE_BUN,
			cargo: environment.WADDLE_FLAKE_CARGO,
			rustc: environment.WADDLE_FLAKE_RUSTC,
			wasmPack: environment.WADDLE_FLAKE_WASM_PACK,
			wasmBindgen: environment.WADDLE_FLAKE_WASM_BINDGEN,
		},
		versions: {
			bun: environment.WADDLE_FLAKE_BUN_VERSION,
			cargo: environment.WADDLE_FLAKE_RUST_VERSION,
			rustc: environment.WADDLE_FLAKE_RUST_VERSION,
			wasmPack: environment.WADDLE_FLAKE_WASM_PACK_VERSION,
			wasmBindgen: environment.WADDLE_FLAKE_WASM_BINDGEN_VERSION,
		},
	};
}

export function assertPinnedWasmBuildFilesystem(environment, outDir) {
	if (!environment.WADDLE_WASM_BUILD_ROOT) {
		throw new Error(
			"WADDLE_WASM_BUILD_ROOT is required in the pinned WASM build",
		);
	}
	const root = realpathSync(environment.WADDLE_WASM_BUILD_ROOT);
	assertContained(root, realpathSync(outDir), "canonical WASM output");
	for (const [name, value] of Object.entries({
		CARGO_HOME: environment.CARGO_HOME,
		CARGO_TARGET_DIR: environment.CARGO_TARGET_DIR,
		HOME: environment.HOME,
	})) {
		if (!value) throw new Error(`${name} is required in the pinned WASM build`);
		const path = realpathSync(value);
		assertContained(root, path, name);
	}
	for (const name of ["config", "config.toml"]) {
		if (existsSync(resolve(environment.CARGO_HOME, name))) {
			throw new Error(`pinned CARGO_HOME must be configuration-free: ${name}`);
		}
	}
	if (readdirSync(environment.CARGO_TARGET_DIR).length !== 0) {
		throw new Error("pinned CARGO_TARGET_DIR must start empty");
	}
}

export function canonicalEncodedRustFlags(
	repoRoot,
	buildRoot,
	remapPathPrefixes,
) {
	for (const [name, path] of Object.entries({ repoRoot, buildRoot })) {
		if (!isAbsolute(path) || path.includes(ENCODED_RUSTFLAG_SEPARATOR)) {
			throw new Error(
				`${name} cannot be encoded as a canonical rustc path prefix`,
			);
		}
	}
	return [
		`--remap-path-prefix=${buildRoot}=${remapPathPrefixes.buildRoot}`,
		`--remap-path-prefix=${repoRoot}=${remapPathPrefixes.repoRoot}`,
	].join(ENCODED_RUSTFLAG_SEPARATOR);
}

export function assertPinnedWasmBuildProcess(
	environment,
	contract,
	outDir,
	repoRoot,
	expectedBuildId,
) {
	const protocol = contract.executor.protocol;
	if (
		protocol !== PINNED_WASM_EXECUTOR_PROTOCOL ||
		environment.WADDLE_WASM_EXECUTOR_PROTOCOL !== protocol
	) {
		throw new Error(
			"canonical WASM build did not enter through the versioned flake executor",
		);
	}
	if (environment.WADDLE_WASM_REPO_ROOT !== repoRoot) {
		throw new Error(
			"canonical WASM build repository root does not match the executor",
		);
	}
	assertWasmBuildIdentityHandoff(environment, expectedBuildId);
	const expectedRustFlags = canonicalEncodedRustFlags(
		repoRoot,
		environment.WADDLE_WASM_BUILD_ROOT,
		contract.executor.remapPathPrefixes,
	);
	if (environment.CARGO_ENCODED_RUSTFLAGS !== expectedRustFlags) {
		throw new Error(
			"canonical WASM build rustc path remapping does not match the contract",
		);
	}
	assertPinnedWasmBuildFilesystem(environment, outDir);

	const expected = expectedToolchain(environment);
	resolvePinnedNixToolchain(expected.executables, expected.versions);
}

export function assertCanonicalWasmBuildId(buildId) {
	if (typeof buildId !== "string" || !CANONICAL_BUILD_ID.test(buildId)) {
		throw new Error(
			"canonical WASM build identity must be exactly 64 lowercase hex characters",
		);
	}
}

export function assertWasmBuildIdentityHandoff(environment, expectedBuildId) {
	assertCanonicalWasmBuildId(expectedBuildId);
	if (environment.WADDLE_WASM_BUILD_ID !== expectedBuildId) {
		throw new Error(
			"canonical WASM build identity handoff does not match the executor",
		);
	}
}

export function pinnedFlakeBuildArgs({
	repoRoot,
	scriptPath,
	outDir,
	paths,
	executor,
	buildId,
}) {
	assertCanonicalWasmBuildId(buildId);
	const encodedRustFlags = canonicalEncodedRustFlags(
		repoRoot,
		paths.root,
		executor.remapPathPrefixes,
	);
	return [
		"develop",
		"--no-update-lock-file",
		"--no-write-lock-file",
		"--ignore-environment",
		`path:${repoRoot}`,
		"--command",
		"env",
		`WADDLE_WASM_EXECUTOR_PROTOCOL=${executor.protocol}`,
		`WADDLE_WASM_BUILD_ROOT=${paths.root}`,
		`WADDLE_WASM_BUILD_ID=${buildId}`,
		`WADDLE_WASM_REPO_ROOT=${repoRoot}`,
		`CARGO_HOME=${paths.cargoHome}`,
		`CARGO_TARGET_DIR=${paths.cargoTarget}`,
		`CARGO_ENCODED_RUSTFLAGS=${encodedRustFlags}`,
		`HOME=${paths.home}`,
		"bun",
		"run",
		scriptPath,
		"--internal-pinned-build",
		outDir,
		buildId,
	];
}

export function runPinnedWasmBuild({
	repoRoot,
	scriptPath,
	outDir,
	paths,
	contract,
	buildId,
	environment = process.env,
}) {
	assertHermeticWasmBuildEnvironment(environment);
	const args = pinnedFlakeBuildArgs({
		repoRoot,
		scriptPath,
		outDir,
		paths,
		executor: contract.executor,
		buildId,
	});
	execFileSync("nix", args, {
		cwd: repoRoot,
		stdio: "inherit",
		env: { ...environment },
	});
}

export function assertWasmArtifactSetsEqual(leftDir, rightDir) {
	const different = [];
	for (const name of WASM_PACKAGE_ARTIFACTS) {
		const left = resolve(leftDir, name);
		const right = resolve(rightDir, name);
		if (
			!existsSync(left) ||
			!existsSync(right) ||
			!readFileSync(left).equals(readFileSync(right))
		) {
			different.push(name);
		}
	}
	if (different.length > 0) {
		throw new Error(
			`isolated canonical WASM builds diverged: ${different.join(", ")}`,
		);
	}
}
