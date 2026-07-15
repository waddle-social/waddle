import { lstatSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import {
	bytewiseCompare,
	canonicalRelativePath,
} from "./wasm-build-input-digest.mjs";

export const CONTRACT_PATH = "chat/scripts/wasm-build-contract.json";
export const PACKAGE_PATH = "chat/package.json";

export const WASM_BUILD_INPUT_MANIFEST = Object.freeze({
	requiredFiles: Object.freeze([
		"chat/scripts/build-and-publish-wasm.mjs",
		"chat/scripts/build-xmpp-wasm.mjs",
		CONTRACT_PATH,
		"chat/scripts/wasm-build-contract.mjs",
		"chat/scripts/wasm-build-environment.mjs",
		"chat/scripts/wasm-build-input-digest.mjs",
		"chat/scripts/wasm-build-input-manifest.mjs",
		"chat/scripts/wasm-build-inputs.mjs",
		"chat/scripts/wasm-package-bindings.mjs",
		"flake.lock",
		"flake.nix",
		"server/Cargo.lock",
		"server/Cargo.toml",
		"server/crates/waddle-xmpp-client-wasm/Cargo.toml",
		"server/crates/waddle-xmpp-client/Cargo.toml",
		"server/crates/waddle-xmpp-core/Cargo.toml",
		"server/rust-toolchain.toml",
	]),
	optionalFiles: Object.freeze([
		"server/.cargo/config",
		"server/.cargo/config.toml",
		"server/crates/waddle-xmpp-client-wasm/build.rs",
		"server/crates/waddle-xmpp-client/build.rs",
		"server/crates/waddle-xmpp-core/build.rs",
	]),
	sourceRoots: Object.freeze([
		"server/crates/waddle-xmpp-client-wasm/src",
		"server/crates/waddle-xmpp-client/src",
		"server/crates/waddle-xmpp-core/src",
	]),
});

const IGNORED_DIRECTORY_NAMES = new Set([
	".git",
	".jj",
	"node_modules",
	"target",
	"wasm-pkg",
]);
const IGNORED_FILE_NAMES = new Set([".DS_Store", "Thumbs.db"]);
const IGNORED_FILE_PATTERNS = Object.freeze([
	/^\.#/u,
	/^#.*#$/u,
	/~$/u,
	/\.(?:orig|rej|swp|swo|temp|tmp)$/u,
]);
const COMPILE_TIME_INCLUDE = /\binclude(?:_bytes|_str)?!\s*\(/u;
const RUST_PATH_OVERRIDE = /#\s*\[\s*path\s*=/u;
const LITERAL_RUST_PATH_OVERRIDE = /#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]/gu;

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

function walkSourceRoot(repoRoot, sourceRoot, declaredFiles, entries) {
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
				if (!IGNORED_DIRECTORY_NAMES.has(name)) visit(relativePath, path);
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
	if (!entries.has(`${canonicalRoot}/lib.rs`)) {
		throw new Error(
			`missing required WASM crate entry point: ${canonicalRoot}/lib.rs`,
		);
	}
}

function validateRustSourceClosure(entries) {
	const decoder = new TextDecoder("utf-8", { fatal: true });
	const includedPaths = new Set(entries.keys());
	for (const [path, bytes] of entries) {
		if (!path.endsWith(".rs")) continue;
		let source;
		try {
			source = decoder.decode(bytes);
		} catch {
			throw new Error(`WASM Rust source must be valid UTF-8: ${path}`);
		}
		if (COMPILE_TIME_INCLUDE.test(source)) {
			throw new Error(
				`unsupported compile-time include macro in ${path}; declare the included input and extend the manifest validator`,
			);
		}
		const pathOverrides = [...source.matchAll(LITERAL_RUST_PATH_OVERRIDE)];
		const sourceWithoutLiteralOverrides = source.replace(
			LITERAL_RUST_PATH_OVERRIDE,
			"",
		);
		if (RUST_PATH_OVERRIDE.test(sourceWithoutLiteralOverrides)) {
			throw new Error(`unsupported dynamic Rust #[path] override in ${path}`);
		}
		const sourceDirectory = path.split("/").slice(0, -1).join("/");
		for (const match of pathOverrides) {
			const referencedPath = canonicalRelativePath(match[1]);
			const resolvedPath = canonicalRelativePath(
				`${sourceDirectory}/${referencedPath}`,
			);
			if (!includedPaths.has(resolvedPath)) {
				throw new Error(
					`Rust #[path] override in ${path} is outside the declared WASM source closure: ${resolvedPath}`,
				);
			}
		}
	}
}

export function collectDeclaredWasmBuildInputs(repoRoot) {
	assertRepositoryRoot(repoRoot);
	const entries = new Map();
	const declaredFiles = new Set([
		...WASM_BUILD_INPUT_MANIFEST.requiredFiles,
		...WASM_BUILD_INPUT_MANIFEST.optionalFiles,
	]);

	for (const path of WASM_BUILD_INPUT_MANIFEST.requiredFiles) {
		addEntry(entries, path, readWasmBuildInputFile(repoRoot, path, true));
	}
	for (const path of WASM_BUILD_INPUT_MANIFEST.optionalFiles) {
		const bytes = readWasmBuildInputFile(repoRoot, path, false);
		if (bytes !== undefined) addEntry(entries, path, bytes);
	}
	for (const sourceRoot of WASM_BUILD_INPUT_MANIFEST.sourceRoots) {
		walkSourceRoot(repoRoot, sourceRoot, declaredFiles, entries);
	}

	validateRustSourceClosure(entries);
	return entries;
}
