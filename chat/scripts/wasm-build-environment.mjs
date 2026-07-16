import { execFileSync } from "node:child_process";
import { X_OK } from "node:constants";
import { accessSync, existsSync, lstatSync, realpathSync } from "node:fs";
import { delimiter, dirname, resolve } from "node:path";
import { bytewiseCompare } from "./wasm-build-input-digest.mjs";

const FORBIDDEN_ENVIRONMENT_NAMES = new Set([
	"CARGO_BUILD_RUSTC",
	"CARGO_BUILD_RUSTC_WRAPPER",
	"CARGO_BUILD_RUSTFLAGS",
	"CARGO_BUILD_TARGET",
	"CARGO_ENCODED_RUSTDOCFLAGS",
	"CARGO_ENCODED_RUSTFLAGS",
	"CARGO_INCREMENTAL",
	"RUSTC",
	"RUSTC_BOOTSTRAP",
	"RUSTC_WRAPPER",
	"RUSTC_WORKSPACE_WRAPPER",
	"RUSTDOCFLAGS",
	"RUSTFLAGS",
	"RUSTUP_TOOLCHAIN",
	"WASM_PACK_PROFILE",
]);
const PINNED_TOOL_NAMES = Object.freeze([
	"bun",
	"cargo",
	"rustc",
	"wasmPack",
	"wasmBindgen",
]);
const CARGO_CONFIG_NAMES = Object.freeze(["config", "config.toml"]);
export const REPOSITORY_CARGO_CONFIG_PATHS = Object.freeze([
	".cargo/config",
	".cargo/config.toml",
	"server/.cargo/config",
	"server/.cargo/config.toml",
	"server/crates/.cargo/config",
	"server/crates/.cargo/config.toml",
	"server/crates/waddle-xmpp-client-wasm/.cargo/config",
	"server/crates/waddle-xmpp-client-wasm/.cargo/config.toml",
]);

function isForbiddenEnvironmentName(name) {
	if (
		FORBIDDEN_ENVIRONMENT_NAMES.has(name) ||
		name.startsWith("CARGO_PROFILE_")
	) {
		return true;
	}
	return /^CARGO_TARGET_(?!DIR$).+_(?:LINKER|RUNNER|RUSTFLAGS)$/u.test(name);
}

export function assertHermeticWasmBuildEnvironment(environment) {
	const forbidden = Object.keys(environment)
		.filter((name) => isForbiddenEnvironmentName(name))
		.sort(bytewiseCompare);
	if (forbidden.length > 0) {
		throw new Error(
			`output-affecting Rust/Cargo environment is not allowed for the canonical WASM build: ${forbidden.join(", ")}`,
		);
	}

	if (environment.CARGO_HOME) {
		for (const name of CARGO_CONFIG_NAMES) {
			if (existsSync(resolve(environment.CARGO_HOME, name))) {
				throw new Error(
					`ambient CARGO_HOME configuration is not allowed for the canonical WASM build: ${name}`,
				);
			}
		}
	}
}

/// Cargo walks `.cargo/config{,.toml}` from the crate working directory to
/// the filesystem root. Repository locations are canonical hashed inputs;
/// any matching ancestor above the repository would be ambient and must make
/// the build fail closed even when CARGO_HOME itself is clean.
export function assertNoAmbientCargoAncestorConfig(repoRoot) {
	let directory = dirname(resolve(repoRoot));
	while (true) {
		for (const name of CARGO_CONFIG_NAMES) {
			const path = resolve(directory, ".cargo", name);
			if (existsSync(path)) {
				throw new Error(
					`ambient Cargo ancestor configuration is not allowed for the canonical WASM build: ${path}`,
				);
			}
		}
		const parent = dirname(directory);
		if (parent === directory) return;
		directory = parent;
	}
}

/// Cargo configuration is executable build policy rather than ordinary source
/// input. The canonical WASM pipeline owns every output-affecting setting, so
/// all repository locations Cargo would discover must stay absent.
export function assertNoRepositoryCargoConfig(repoRoot) {
	for (const relativePath of REPOSITORY_CARGO_CONFIG_PATHS) {
		const path = resolve(repoRoot, ...relativePath.split("/"));
		try {
			lstatSync(path);
		} catch (error) {
			if (error?.code === "ENOENT") continue;
			throw error;
		}
		throw new Error(
			`repository Cargo configuration is not allowed for the canonical WASM build: ${relativePath}`,
		);
	}
}

export function assertPinnedNixToolchain(executables, expectedExecutables) {
	for (const name of PINNED_TOOL_NAMES) {
		const path = executables[name];
		const expected = expectedExecutables[name];
		if (
			typeof path !== "string" ||
			typeof expected !== "string" ||
			!expected.startsWith("/nix/store/") ||
			path !== expected
		) {
			throw new Error(
				`${name} must resolve to the exact repository-flake tool: expected ${expected}, got ${path}`,
			);
		}
	}
}

export function assertPinnedToolVersions(versions, expectedVersions) {
	for (const name of PINNED_TOOL_NAMES) {
		if (versions[name] !== expectedVersions[name]) {
			throw new Error(
				`${name} version must match the repository flake: expected ${expectedVersions[name]}, got ${versions[name]}`,
			);
		}
	}
}

export function resolveExecutableOnPath(name, pathValue = process.env.PATH) {
	if (!pathValue) throw new Error(`PATH is required to resolve ${name}`);
	for (const directory of pathValue.split(delimiter)) {
		if (!directory) continue;
		const candidate = resolve(directory, name);
		try {
			accessSync(candidate, X_OK);
			return realpathSync(candidate);
		} catch {
			// Keep searching the explicitly provided PATH.
		}
	}
	throw new Error(`could not resolve ${name} on the pinned flake PATH`);
}

export function resolvePinnedNixToolchain(
	expectedExecutables,
	expectedVersions,
	bunPath = process.execPath,
) {
	const executables = {
		bun: realpathSync(bunPath),
		cargo: resolveExecutableOnPath("cargo"),
		rustc: resolveExecutableOnPath("rustc"),
		wasmPack: resolveExecutableOnPath("wasm-pack"),
		wasmBindgen: resolveExecutableOnPath("wasm-bindgen"),
	};
	const normalizedExpectedExecutables = Object.fromEntries(
		Object.entries(expectedExecutables).map(([name, path]) => [
			name,
			realpathSync(path),
		]),
	);
	assertPinnedNixToolchain(executables, normalizedExpectedExecutables);
	const version = (executable) => {
		const output = execFileSync(executable, ["--version"], {
			encoding: "utf8",
		}).trim();
		const match = output.match(/\b\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?\b/u);
		if (!match) throw new Error(`could not parse tool version from: ${output}`);
		return match[0];
	};
	const versions = {
		bun: version(executables.bun),
		cargo: version(executables.cargo),
		rustc: version(executables.rustc),
		wasmPack: version(executables.wasmPack),
		wasmBindgen: version(executables.wasmBindgen),
	};
	assertPinnedToolVersions(versions, expectedVersions);
	return { executables, versions };
}
