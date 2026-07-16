import { execFileSync } from "node:child_process";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { canonicalRelativePath } from "./wasm-build-input-digest.mjs";

const CARGO_METADATA_FORMAT_VERSION = 1;
const LOCAL_CRATE_ROOT = "server/crates";
const LIBRARY_TARGET_KINDS = new Set([
	"cdylib",
	"dylib",
	"lib",
	"proc-macro",
	"rlib",
	"staticlib",
]);
const RESOLVED_DEPENDENCY_KINDS = new Set([null, "normal", "build"]);
const KNOWN_DEPENDENCY_KINDS = new Set([...RESOLVED_DEPENDENCY_KINDS, "dev"]);
const LOCK_BACKED_SOURCE_PREFIXES = Object.freeze([
	"git+",
	"registry+",
	"sparse+",
]);

function relativeRepositoryPath(repoRoot, absolutePath, description) {
	if (typeof absolutePath !== "string" || !isAbsolute(absolutePath)) {
		throw new Error(`${description} must be an absolute path: ${absolutePath}`);
	}
	const relativePath = relative(repoRoot, absolutePath);
	if (
		relativePath === "" ||
		relativePath === ".." ||
		relativePath.startsWith(`..${sep}`) ||
		isAbsolute(relativePath)
	) {
		throw new Error(`${description} escapes the repository: ${absolutePath}`);
	}
	return canonicalRelativePath(relativePath.split(sep).join("/"));
}

function relativePackagePath(packageRoot, absolutePath, description) {
	if (typeof absolutePath !== "string" || !isAbsolute(absolutePath)) {
		throw new Error(`${description} must be an absolute path: ${absolutePath}`);
	}
	const relativePath = relative(packageRoot, absolutePath);
	if (
		relativePath === "" ||
		relativePath === ".." ||
		relativePath.startsWith(`..${sep}`) ||
		isAbsolute(relativePath)
	) {
		throw new Error(
			`${description} escapes its Cargo package: ${absolutePath}`,
		);
	}
	return canonicalRelativePath(relativePath.split(sep).join("/"));
}

export function parseWasmCargoMetadata(stdout) {
	let metadata;
	try {
		metadata = JSON.parse(stdout);
	} catch (error) {
		throw new Error(`invalid Cargo metadata JSON: ${error.message}`);
	}
	if (metadata?.version !== CARGO_METADATA_FORMAT_VERSION) {
		throw new Error("unsupported Cargo metadata format version");
	}
	if (
		!Array.isArray(metadata.packages) ||
		!Array.isArray(metadata.resolve?.nodes)
	) {
		throw new Error("Cargo metadata is missing the resolved package graph");
	}
	return metadata;
}

export function loadWasmCargoMetadata(
	repoRoot,
	{
		cargo = "cargo",
		execute = execFileSync,
		target = "wasm32-unknown-unknown",
	} = {},
) {
	const stdout = execute(
		cargo,
		[
			"metadata",
			"--format-version",
			String(CARGO_METADATA_FORMAT_VERSION),
			"--locked",
			"--manifest-path",
			resolve(repoRoot, "server/Cargo.toml"),
			"--filter-platform",
			target,
		],
		{
			cwd: repoRoot,
			encoding: "utf8",
			maxBuffer: 64 * 1024 * 1024,
			stdio: ["ignore", "pipe", "inherit"],
		},
	);
	return parseWasmCargoMetadata(stdout);
}

function packageMaps(metadata) {
	const packages = new Map();
	for (const pkg of metadata.packages) {
		if (!pkg || typeof pkg.id !== "string" || pkg.id.length === 0) {
			throw new Error("Cargo metadata contains a package without an identity");
		}
		if (packages.has(pkg.id)) {
			throw new Error(
				`Cargo metadata contains duplicate package identity: ${pkg.id}`,
			);
		}
		packages.set(pkg.id, pkg);
	}

	const nodes = new Map();
	for (const node of metadata.resolve.nodes) {
		if (!node || typeof node.id !== "string" || node.id.length === 0) {
			throw new Error(
				"Cargo metadata contains a resolved node without an identity",
			);
		}
		if (nodes.has(node.id)) {
			throw new Error(
				`Cargo metadata contains duplicate resolved node: ${node.id}`,
			);
		}
		nodes.set(node.id, node);
	}
	return { packages, nodes };
}

function buildRelevantDependencyIds(node) {
	if (!Array.isArray(node.deps)) {
		throw new Error(`Cargo resolved node is missing dependencies: ${node.id}`);
	}
	const dependencies = [];
	for (const dependency of node.deps) {
		if (!dependency || typeof dependency.pkg !== "string") {
			throw new Error(
				`Cargo dependency is missing a package identity: ${node.id}`,
			);
		}
		if (
			!Array.isArray(dependency.dep_kinds) ||
			dependency.dep_kinds.length === 0
		) {
			throw new Error(
				`Cargo dependency is missing dependency kinds: ${dependency.pkg}`,
			);
		}
		let buildRelevant = false;
		for (const dependencyKind of dependency.dep_kinds) {
			if (!dependencyKind || !KNOWN_DEPENDENCY_KINDS.has(dependencyKind.kind)) {
				throw new Error(
					`unsupported Cargo dependency kind for ${dependency.pkg}`,
				);
			}
			if (RESOLVED_DEPENDENCY_KINDS.has(dependencyKind.kind)) {
				buildRelevant = true;
			}
		}
		if (buildRelevant) dependencies.push(dependency.pkg);
	}
	return dependencies;
}

function reachablePackageIds(rootId, packages, nodes) {
	const pending = [rootId];
	const visited = new Set();
	while (pending.length > 0) {
		const packageId = pending.pop();
		if (visited.has(packageId)) continue;
		if (!packages.has(packageId)) {
			throw new Error(
				`resolved Cargo package is missing metadata: ${packageId}`,
			);
		}
		const node = nodes.get(packageId);
		if (!node) {
			throw new Error(
				`Cargo package is missing a resolved graph node: ${packageId}`,
			);
		}
		visited.add(packageId);
		pending.push(...buildRelevantDependencyIds(node));
	}
	return visited;
}

function assertLockBackedSource(pkg) {
	if (
		typeof pkg.source !== "string" ||
		!LOCK_BACKED_SOURCE_PREFIXES.some((prefix) => pkg.source.startsWith(prefix))
	) {
		throw new Error(`unsupported Cargo package source identity: ${pkg.id}`);
	}
}

function isLibraryTarget(target) {
	return (
		Array.isArray(target.kind) &&
		target.kind.some((kind) => LIBRARY_TARGET_KINDS.has(kind))
	);
}

function localPackageInputs(repoRoot, pkg) {
	const manifestPath = relativeRepositoryPath(
		repoRoot,
		pkg.manifest_path,
		`local Cargo manifest for ${pkg.id}`,
	);
	if (
		!manifestPath.startsWith(`${LOCAL_CRATE_ROOT}/`) ||
		!manifestPath.endsWith("/Cargo.toml")
	) {
		throw new Error(
			`local Cargo package must live under ${LOCAL_CRATE_ROOT}: ${manifestPath}`,
		);
	}
	const packageRoot = dirname(resolve(repoRoot, ...manifestPath.split("/")));
	if (!Array.isArray(pkg.targets) || pkg.targets.length === 0) {
		throw new Error(`local Cargo package has no targets: ${manifestPath}`);
	}

	const requiredFiles = [manifestPath];
	const sourceEntries = [];
	for (const target of pkg.targets) {
		if (!target || !Array.isArray(target.kind)) {
			throw new Error(
				`local Cargo package has a malformed target: ${manifestPath}`,
			);
		}
		const isBuildScript = target.kind.includes("custom-build");
		if (!isBuildScript && !isLibraryTarget(target)) continue;
		relativePackagePath(
			packageRoot,
			target.src_path,
			`Cargo target for ${manifestPath}`,
		);
		const targetPath = relativeRepositoryPath(
			repoRoot,
			target.src_path,
			`Cargo target for ${manifestPath}`,
		);
		if (isBuildScript) {
			requiredFiles.push(targetPath);
		} else {
			sourceEntries.push(targetPath);
		}
	}
	if (sourceEntries.length === 0) {
		throw new Error(
			`local Cargo package has no buildable library target: ${manifestPath}`,
		);
	}
	return { requiredFiles, sourceEntries };
}

export function resolveWasmCargoInputs(
	repoRoot,
	metadata,
	{ rootCrate = "server/crates/waddle-xmpp-client-wasm" } = {},
) {
	if (metadata?.version !== CARGO_METADATA_FORMAT_VERSION) {
		throw new Error("unsupported Cargo metadata format version");
	}
	const { packages, nodes } = packageMaps(metadata);
	const rootManifest = `${canonicalRelativePath(rootCrate)}/Cargo.toml`;
	const roots = [...packages.values()].filter((pkg) => {
		try {
			return (
				pkg.source === null &&
				relativeRepositoryPath(
					repoRoot,
					pkg.manifest_path,
					"Cargo manifest",
				) === rootManifest
			);
		} catch {
			return false;
		}
	});
	if (roots.length !== 1) {
		throw new Error(
			`Cargo metadata must contain exactly one root package: ${rootManifest}`,
		);
	}

	const requiredFiles = new Set();
	const sourceEntries = new Set();
	for (const packageId of reachablePackageIds(roots[0].id, packages, nodes)) {
		const pkg = packages.get(packageId);
		if (pkg.source !== null) {
			assertLockBackedSource(pkg);
			continue;
		}
		const inputs = localPackageInputs(repoRoot, pkg);
		for (const path of inputs.requiredFiles) requiredFiles.add(path);
		for (const path of inputs.sourceEntries) sourceEntries.add(path);
	}
	return {
		requiredFiles: [...requiredFiles].sort(),
		sourceEntries: [...sourceEntries].sort(),
	};
}
