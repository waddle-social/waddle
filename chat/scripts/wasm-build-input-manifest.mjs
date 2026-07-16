import { lstatSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import {
	bytewiseCompare,
	canonicalRelativePath,
} from "./wasm-build-input-digest.mjs";
import { loadPinnedWasmCargoMetadata } from "./wasm-cargo-metadata-executor.mjs";
import { resolveWasmCargoInputs } from "./wasm-cargo-metadata.mjs";
import { validateWasmBuildModuleClosure } from "./wasm-build-module-closure.mjs";
import { validateWasmRustSourceClosure } from "./wasm-rust-source-closure.mjs";
import { assertNoRepositoryCargoConfig } from "./wasm-build-environment.mjs";

export const CONTRACT_PATH = "chat/scripts/wasm-build-contract.json";
export const PACKAGE_PATH = "chat/package.json";

export const WASM_BUILD_INPUT_MANIFEST = Object.freeze({
	requiredFiles: Object.freeze([
		"chat/scripts/build-and-publish-wasm.mjs",
		"chat/scripts/build-xmpp-wasm.mjs",
		CONTRACT_PATH,
		"chat/scripts/wasm-cargo-metadata-executor.mjs",
		"chat/scripts/wasm-cargo-metadata.mjs",
		"chat/scripts/wasm-build-contract.mjs",
		"chat/scripts/wasm-build-environment.mjs",
		"chat/scripts/wasm-build-executor.mjs",
		"chat/scripts/wasm-build-input-digest.mjs",
		"chat/scripts/wasm-build-input-manifest.mjs",
		"chat/scripts/wasm-build-inputs.mjs",
		"chat/scripts/wasm-build-module-closure.mjs",
		"chat/scripts/wasm-package-bindings.mjs",
		"chat/scripts/wasm-rust-source-closure.mjs",
		"flake.lock",
		"flake.nix",
		"server/Cargo.lock",
		"server/Cargo.toml",
		"server/rust-toolchain.toml",
	]),
	optionalFiles: Object.freeze([]),
});

const BUILD_MODULE_PATHS = Object.freeze(
	WASM_BUILD_INPUT_MANIFEST.requiredFiles.filter(
		(path) => path.startsWith("chat/scripts/") && path.endsWith(".mjs"),
	),
);
const IGNORED_FILE_NAMES = new Set([".DS_Store", "Thumbs.db"]);
const IGNORED_FILE_PATTERNS = Object.freeze([
	/^\.#/u,
	/^#.*#$/u,
	/~$/u,
	/\.(?:orig|rej|swp|swo|temp|tmp)$/u,
]);

function absolutePath(repoRoot, relativePath) {
	return resolve(repoRoot, ...canonicalRelativePath(relativePath).split("/"));
}

function describeKind(stat) {
	if (stat.isSymbolicLink()) return "symbolic link";
	if (stat.isDirectory()) return "directory";
	if (stat.isFile()) return "regular file";
	return "special file";
}

function requiredStat(path, description) {
	try {
		return lstatSync(path);
	} catch (error) {
		if (error?.code === "ENOENT") {
			throw new Error(`missing required WASM build ${description}: ${path}`);
		}
		throw error;
	}
}

function assertRepositoryRoot(repoRoot) {
	const stat = requiredStat(repoRoot, "repository root");
	if (stat.isSymbolicLink() || !stat.isDirectory()) {
		throw new Error(
			`WASM build repository root must be a real directory: ${repoRoot}`,
		);
	}
}

function assertDirectoryAncestors(repoRoot, relativePath) {
	assertRepositoryRoot(repoRoot);
	const segments = canonicalRelativePath(relativePath).split("/").slice(0, -1);
	let current = repoRoot;
	for (const segment of segments) {
		current = resolve(current, segment);
		const stat = requiredStat(current, "input ancestor");
		if (stat.isSymbolicLink() || !stat.isDirectory()) {
			throw new Error(
				`WASM build input ancestor must be a real directory: ${current} (${describeKind(stat)})`,
			);
		}
	}
}

export function readWasmBuildInputFile(repoRoot, relativePath, required) {
	const canonicalPath = canonicalRelativePath(relativePath);
	const path = absolutePath(repoRoot, canonicalPath);
	let stat;
	try {
		stat = lstatSync(path);
	} catch (error) {
		if (!required && error?.code === "ENOENT") return undefined;
		if (error?.code === "ENOENT") {
			throw new Error(`missing required WASM build input: ${canonicalPath}`);
		}
		throw error;
	}
	assertDirectoryAncestors(repoRoot, canonicalPath);
	if (stat.isSymbolicLink() || !stat.isFile()) {
		throw new Error(
			`WASM build input must be a regular file: ${canonicalPath} (${describeKind(stat)})`,
		);
	}
	return readFileSync(path);
}

function addEntry(entries, path, bytes) {
	const canonicalPath = canonicalRelativePath(path);
	if (entries.has(canonicalPath)) {
		throw new Error(`duplicate WASM build input path: ${canonicalPath}`);
	}
	entries.set(canonicalPath, bytes);
}

function isIgnoredFile(name) {
	return (
		IGNORED_FILE_NAMES.has(name) ||
		IGNORED_FILE_PATTERNS.some((pattern) => pattern.test(name))
	);
}

function walkSourceRoot(
	repoRoot,
	sourceRoot,
	entryPoints,
	declaredFiles,
	entries,
) {
	const canonicalRoot = canonicalRelativePath(sourceRoot);
	const rootPath = absolutePath(repoRoot, canonicalRoot);
	assertDirectoryAncestors(repoRoot, `${canonicalRoot}/placeholder`);
	const rootStat = requiredStat(rootPath, "source root");
	if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
		throw new Error(
			`WASM build source root must be a real directory: ${canonicalRoot} (${describeKind(rootStat)})`,
		);
	}

	function visit(relativeDirectory, directoryPath) {
		const names = readdirSync(directoryPath).sort(bytewiseCompare);
		for (const name of names) {
			const relativePath = `${relativeDirectory}/${name}`;
			const path = resolve(directoryPath, name);
			const stat = lstatSync(path);
			if (stat.isSymbolicLink()) {
				throw new Error(
					`WASM build source path must not be a symbolic link: ${relativePath}`,
				);
			}
			if (stat.isDirectory()) {
				visit(relativePath, path);
				continue;
			}
			if (!stat.isFile()) {
				throw new Error(
					`WASM build source path must be a regular file: ${relativePath}`,
				);
			}
			if (isIgnoredFile(name)) continue;
			if (declaredFiles.has(relativePath)) continue;
			if (!name.endsWith(".rs")) {
				throw new Error(
					`unsupported WASM build source input: ${relativePath}; declare it explicitly or classify it as a known temporary file`,
				);
			}
			addEntry(entries, relativePath, readFileSync(path));
		}
	}

	visit(canonicalRoot, rootPath);
	for (const entryPoint of entryPoints) {
		if (!entries.has(entryPoint)) {
			throw new Error(`missing required WASM crate entry point: ${entryPoint}`);
		}
	}
}
export function collectDeclaredWasmBuildInputs(repoRoot, options = {}) {
	assertRepositoryRoot(repoRoot);
	assertNoRepositoryCargoConfig(repoRoot);
	const cargoMetadata =
		options.cargoMetadata ??
		(options.loadCargoMetadata ?? loadPinnedWasmCargoMetadata)(repoRoot);
	const entries = new Map();
	const cargoInputs = resolveWasmCargoInputs(repoRoot, cargoMetadata);
	const declaredFiles = new Set([
		...WASM_BUILD_INPUT_MANIFEST.requiredFiles,
		...WASM_BUILD_INPUT_MANIFEST.optionalFiles,
		...cargoInputs.requiredFiles,
	]);

	for (const path of WASM_BUILD_INPUT_MANIFEST.requiredFiles) {
		addEntry(entries, path, readWasmBuildInputFile(repoRoot, path, true));
	}
	for (const path of WASM_BUILD_INPUT_MANIFEST.optionalFiles) {
		const bytes = readWasmBuildInputFile(repoRoot, path, false);
		if (bytes !== undefined) addEntry(entries, path, bytes);
	}
	for (const path of cargoInputs.requiredFiles) {
		addEntry(entries, path, readWasmBuildInputFile(repoRoot, path, true));
	}

	const sourceRoots = new Map();
	for (const entryPoint of cargoInputs.sourceEntries) {
		const sourceRoot = entryPoint.slice(0, entryPoint.lastIndexOf("/"));
		const entryPoints = sourceRoots.get(sourceRoot) ?? [];
		entryPoints.push(entryPoint);
		sourceRoots.set(sourceRoot, entryPoints);
	}
	for (const [sourceRoot, entryPoints] of sourceRoots) {
		walkSourceRoot(repoRoot, sourceRoot, entryPoints, declaredFiles, entries);
	}

	validateWasmRustSourceClosure(entries);
	validateWasmBuildModuleClosure(entries, BUILD_MODULE_PATHS);
	return entries;
}
