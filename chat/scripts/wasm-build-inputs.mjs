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
	assertPinnedNixToolchain,
	assertPinnedToolVersions,
	resolveExecutableOnPath,
	resolvePinnedNixToolchain,
} from "./wasm-build-environment.mjs";
export { WASM_BUILD_INPUT_MANIFEST };

export function collectWasmBuildInputs(repoRoot) {
	const inputMap = collectDeclaredWasmBuildInputs(repoRoot);
	const contract = parseWasmBuildContract(inputMap.get(CONTRACT_PATH));
	const packageBytes = readWasmBuildInputFile(repoRoot, PACKAGE_PATH, true);
	validateWasmPackageScript(packageBytes, PACKAGE_PATH, contract);
	return {
		contract,
		entries: [...inputMap].map(([path, bytes]) => ({ path, bytes })),
	};
}

export function canonicalWasmBuildIdentity(repoRoot) {
	const { contract, entries } = collectWasmBuildInputs(repoRoot);
	return {
		buildId: digestCanonicalInputs(entries, contract.digestFormat),
		contract,
		entries,
	};
}
