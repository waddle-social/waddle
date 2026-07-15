import { execFileSync } from "node:child_process";
import { realpathSync } from "node:fs";
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
}

export function assertPinnedNixToolchain(executables) {
	for (const [name, path] of Object.entries(executables)) {
		if (typeof path !== "string" || !path.startsWith("/nix/store/")) {
			throw new Error(
				`${name} must resolve from the flake-pinned /nix/store toolchain`,
			);
		}
	}
}

export function resolvePinnedNixToolchain(bunPath = process.execPath) {
	const resolveExecutable = (name) =>
		realpathSync(execFileSync("which", [name], { encoding: "utf8" }).trim());
	const executables = {
		bun: realpathSync(bunPath),
		cargo: resolveExecutable("cargo"),
		rustc: resolveExecutable("rustc"),
		wasmPack: resolveExecutable("wasm-pack"),
	};
	assertPinnedNixToolchain(executables);
	return executables;
}
