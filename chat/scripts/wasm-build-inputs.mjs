import {
	parseWasmBuildContract,
	validateWasmPackageScript,
} from "./wasm-build-contract.mjs";
import { digestCanonicalInputs } from "./wasm-build-input-digest.mjs";
import {
	CONTRACT_PATH,
	PACKAGE_PATH,
	WASM_BUILD_INPUT_MANIFEST,
	collectDeclaredWasmBuildInputs,
	readWasmBuildInputFile,
} from "./wasm-build-input-manifest.mjs";

export { wasmPackBuildArgs } from "./wasm-build-contract.mjs";
export {
	assertHermeticWasmBuildEnvironment,
	assertNoAmbientCargoAncestorConfig,
	assertNoRepositoryCargoConfig,
	REPOSITORY_CARGO_CONFIG_PATHS,
	assertPinnedNixToolchain,
	assertPinnedToolVersions,
	resolveExecutableOnPath,
	resolvePinnedNixToolchain,
} from "./wasm-build-environment.mjs";
export { WASM_BUILD_INPUT_MANIFEST };

export function collectWasmBuildInputs(repoRoot, options = {}) {
	const inputMap = collectDeclaredWasmBuildInputs(repoRoot, options);
	const contract = parseWasmBuildContract(inputMap.get(CONTRACT_PATH));
	const packageBytes = readWasmBuildInputFile(repoRoot, PACKAGE_PATH, true);
	validateWasmPackageScript(packageBytes, PACKAGE_PATH, contract);
	return {
		contract,
		entries: [...inputMap].map(([path, bytes]) => ({ path, bytes })),
	};
}

export function canonicalWasmBuildIdentity(repoRoot, options = {}) {
	const { contract, entries } = collectWasmBuildInputs(repoRoot, options);
	return {
		buildId: digestCanonicalInputs(entries, contract.digestFormat),
		contract,
		entries,
	};
}
