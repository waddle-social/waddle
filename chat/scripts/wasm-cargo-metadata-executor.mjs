import { execFileSync } from "node:child_process";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { isAbsolute, relative, resolve, sep } from "node:path";
import {
	assertHermeticWasmBuildEnvironment,
	assertNoAmbientCargoAncestorConfig,
	resolvePinnedNixToolchain,
} from "./wasm-build-environment.mjs";
import {
	loadWasmCargoMetadata,
	parseWasmCargoMetadata,
} from "./wasm-cargo-metadata.mjs";

export const PINNED_CARGO_METADATA_PROTOCOL =
	"waddle:xmpp-client-wasm:pinned-cargo-metadata:v1";

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

function assertContained(root, path, description) {
	const relativePath = relative(root, path);
	if (
		relativePath === "" ||
		relativePath === ".." ||
		relativePath.startsWith(`..${sep}`) ||
		isAbsolute(relativePath)
	) {
		throw new Error(`${description} must be inside ${root}: ${path}`);
	}
}

export function assertPinnedCargoMetadataProcess(
	environment,
	repoRoot,
	outputPath,
) {
	if (
		environment.WADDLE_WASM_METADATA_PROTOCOL !==
			PINNED_CARGO_METADATA_PROTOCOL ||
		environment.WADDLE_WASM_REPO_ROOT !== repoRoot
	) {
		throw new Error(
			"Cargo metadata did not enter through the versioned pinned-flake executor",
		);
	}
	const runRoot = environment.WADDLE_WASM_METADATA_ROOT;
	if (!runRoot || !isAbsolute(runRoot)) {
		throw new Error("WADDLE_WASM_METADATA_ROOT is required");
	}
	assertContained(runRoot, outputPath, "Cargo metadata output");
	assertHermeticWasmBuildEnvironment(environment);
	const expected = expectedToolchain(environment);
	resolvePinnedNixToolchain(expected.executables, expected.versions);
}

export function pinnedCargoMetadataArgs({
	repoRoot,
	scriptPath,
	outputPath,
	runRoot,
	cargoHome,
	home,
}) {
	return [
		"develop",
		"--no-update-lock-file",
		"--no-write-lock-file",
		"--ignore-environment",
		`path:${repoRoot}`,
		"--command",
		"env",
		`WADDLE_WASM_METADATA_PROTOCOL=${PINNED_CARGO_METADATA_PROTOCOL}`,
		`WADDLE_WASM_METADATA_ROOT=${runRoot}`,
		`WADDLE_WASM_REPO_ROOT=${repoRoot}`,
		`CARGO_HOME=${cargoHome}`,
		`HOME=${home}`,
		"bun",
		"run",
		scriptPath,
		"--internal-pinned-metadata",
		outputPath,
	];
}

export function loadPinnedWasmCargoMetadata(
	repoRoot,
	{
		environment = process.env,
		execute = execFileSync,
		scriptPath = resolve(
			repoRoot,
			"chat/scripts/wasm-cargo-metadata-executor.mjs",
		),
	} = {},
) {
	assertNoAmbientCargoAncestorConfig(repoRoot);
	const cargoHome = resolve(
		environment.CARGO_HOME ?? resolve(environment.HOME ?? homedir(), ".cargo"),
	);
	mkdirSync(cargoHome, { recursive: true });
	assertHermeticWasmBuildEnvironment({ ...environment, CARGO_HOME: cargoHome });
	const runRoot = mkdtempSync(resolve(tmpdir(), "waddle-wasm-metadata-"));
	const home = resolve(runRoot, "home");
	const outputPath = resolve(runRoot, "cargo-metadata.json");
	mkdirSync(home);
	try {
		execute(
			"nix",
			pinnedCargoMetadataArgs({
				repoRoot,
				scriptPath,
				outputPath,
				runRoot,
				cargoHome,
				home,
			}),
			{
				cwd: repoRoot,
				stdio: "inherit",
				env: { ...environment },
			},
		);
		if (!existsSync(outputPath)) {
			throw new Error("pinned Cargo metadata executor produced no output");
		}
		return parseWasmCargoMetadata(readFileSync(outputPath, "utf8"));
	} finally {
		rmSync(runRoot, { recursive: true, force: true });
	}
}

if (import.meta.main) {
	const internalIndex = process.argv.indexOf("--internal-pinned-metadata");
	const outputPath =
		internalIndex >= 0 ? process.argv[internalIndex + 1] : undefined;
	if (!outputPath || internalIndex !== process.argv.length - 2) {
		throw new Error("invalid internal pinned Cargo metadata invocation");
	}
	const repoRoot = process.env.WADDLE_WASM_REPO_ROOT;
	if (!repoRoot) throw new Error("WADDLE_WASM_REPO_ROOT is required");
	assertPinnedCargoMetadataProcess(process.env, repoRoot, outputPath);
	const metadata = loadWasmCargoMetadata(repoRoot, {
		cargo: process.env.WADDLE_FLAKE_CARGO,
	});
	writeFileSync(outputPath, JSON.stringify(metadata));
}
