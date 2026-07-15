import { posix } from "node:path";
import { canonicalRelativePath } from "./wasm-build-input-digest.mjs";

const MODULE_IMPORT_SCANNER = new Bun.Transpiler({ loader: "js" });
const DYNAMIC_IMPORT_SYNTAX = new RegExp(
	String.raw`\bim` +
		String.raw`port(?:\s|\/\*[\s\S]*?\*\/|\/\/[^\r\n]*(?:\r?\n|$))*\(`,
	"gu",
);
const SUPPORTED_IMPORT_KINDS = new Set(["dynamic-import", "import-statement"]);

function isExplicitBuiltin(specifier) {
	return (
		(specifier.startsWith("node:") && specifier.length > "node:".length) ||
		(specifier.startsWith("bun:") && specifier.length > "bun:".length)
	);
}

function resolveRelativeModulePath(importerPath, specifier) {
	if (specifier.includes("\\")) {
		throw new Error(
			`WASM build module import must use '/': ${importerPath} -> ${specifier}`,
		);
	}
	if (specifier.includes("?") || specifier.includes("#")) {
		throw new Error(
			`WASM build module import must not contain a query or fragment: ${importerPath} -> ${specifier}`,
		);
	}
	if (!specifier.endsWith(".mjs")) {
		throw new Error(
			`WASM build module import must name an explicit .mjs file: ${importerPath} -> ${specifier}`,
		);
	}

	const resolved = posix.normalize(
		posix.join(posix.dirname(importerPath), specifier),
	);
	return canonicalRelativePath(resolved);
}

export function validateWasmBuildModuleClosure(entries, modulePaths) {
	const decoder = new TextDecoder("utf-8", { fatal: true });
	const declaredModules = new Set();
	for (const path of modulePaths) {
		const canonicalPath = canonicalRelativePath(path);
		if (declaredModules.has(canonicalPath)) {
			throw new Error(`duplicate declared WASM build module: ${canonicalPath}`);
		}
		if (!entries.has(canonicalPath)) {
			throw new Error(`missing declared WASM build module: ${canonicalPath}`);
		}
		declaredModules.add(canonicalPath);
	}

	for (const modulePath of declaredModules) {
		let source;
		try {
			source = decoder.decode(entries.get(modulePath));
		} catch {
			throw new Error(`WASM build module must be valid UTF-8: ${modulePath}`);
		}

		let imports;
		try {
			imports = MODULE_IMPORT_SCANNER.scanImports(source);
		} catch (error) {
			throw new Error(
				`invalid WASM build module syntax in ${modulePath}: ${error.message}`,
			);
		}
		const parsedDynamicImports = imports.filter(
			({ kind }) => kind === "dynamic-import",
		).length;
		const dynamicImportExpressions = [...source.matchAll(DYNAMIC_IMPORT_SYNTAX)]
			.length;
		if (dynamicImportExpressions !== parsedDynamicImports) {
			throw new Error(
				`nonliteral or unparseable dynamic import in WASM build module: ${modulePath}`,
			);
		}

		for (const { kind, path: specifier } of imports) {
			if (!SUPPORTED_IMPORT_KINDS.has(kind)) {
				throw new Error(
					`unsupported ${kind} in WASM build module: ${modulePath}`,
				);
			}
			if (isExplicitBuiltin(specifier)) continue;
			if (!specifier.startsWith("./") && !specifier.startsWith("../")) {
				throw new Error(
					`bare or nonlocal import is not bound by the WASM build contract: ${modulePath} -> ${specifier}`,
				);
			}

			const resolvedPath = resolveRelativeModulePath(modulePath, specifier);
			if (!declaredModules.has(resolvedPath)) {
				throw new Error(
					`relative WASM build module import is not declared and hashed: ${modulePath} -> ${resolvedPath}`,
				);
			}
		}
	}
}
