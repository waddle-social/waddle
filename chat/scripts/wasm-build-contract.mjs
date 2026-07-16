const WASM_PACK_ARGS_V2 = Object.freeze([
	"build",
	"--target",
	"bundler",
	"--release",
	"--out-dir",
	"{outDir}",
	"--",
	"--locked",
]);

export function validateWasmBuildContract(contract) {
	if (contract?.schemaVersion !== 2) {
		throw new Error(
			"unsupported WASM build contract schemaVersion; update the validator explicitly",
		);
	}
	if (
		typeof contract.digestFormat !== "string" ||
		!contract.digestFormat.startsWith(
			"waddle:xmpp-client-wasm:canonical-inputs:v",
		)
	) {
		throw new Error("invalid WASM build digest format tag");
	}
	if (contract.packageScript !== "bun run scripts/build-xmpp-wasm.mjs") {
		throw new Error("unsupported WASM package-script contract");
	}
	if (contract.crate !== "server/crates/waddle-xmpp-client-wasm") {
		throw new Error("unsupported WASM crate contract");
	}
	if (
		contract.cargo?.profile !== "release" ||
		contract.cargo?.target !== "wasm32-unknown-unknown" ||
		contract.cargo?.defaultFeatures !== true ||
		!Array.isArray(contract.cargo?.features) ||
		contract.cargo.features.length !== 0 ||
		contract.cargo?.locked !== true
	) {
		throw new Error(
			"unsupported WASM Cargo profile, target, feature, or lock contract",
		);
	}
	if (
		contract.wasmPack?.command !== "wasm-pack" ||
		!Array.isArray(contract.wasmPack?.args) ||
		contract.wasmPack.args.length !== WASM_PACK_ARGS_V2.length ||
		contract.wasmPack.args.some(
			(argument, index) => argument !== WASM_PACK_ARGS_V2[index],
		)
	) {
		throw new Error(
			"unsupported v2 wasm-pack invocation contract; update the versioned validator explicitly",
		);
	}
	if (
		contract.executor?.protocol !==
			"waddle:xmpp-client-wasm:pinned-flake-executor:v1" ||
		contract.executor?.flake !== "path:." ||
		contract.executor?.ignoreEnvironment !== true ||
		contract.executor?.cleanCargoHome !== true ||
		contract.executor?.cleanCargoTarget !== true ||
		contract.executor?.artifactCount !== 6 ||
		contract.executor?.remapPathPrefixes?.buildRoot !== "/waddle-build" ||
		contract.executor?.remapPathPrefixes?.repoRoot !== "/waddle"
	) {
		throw new Error(
			"unsupported canonical WASM flake-executor contract; update the versioned validator explicitly",
		);
	}
}

export function parseWasmBuildContract(bytes) {
	let contract;
	try {
		contract = JSON.parse(Buffer.from(bytes).toString("utf8"));
	} catch (error) {
		throw new Error(`invalid WASM build contract JSON: ${error.message}`);
	}
	validateWasmBuildContract(contract);
	return contract;
}

export function validateWasmPackageScript(bytes, packagePath, contract) {
	let packageJson;
	try {
		packageJson = JSON.parse(Buffer.from(bytes).toString("utf8"));
	} catch (error) {
		throw new Error(`invalid ${packagePath}: ${error.message}`);
	}
	if (packageJson.scripts?.["wasm:build"] !== contract.packageScript) {
		throw new Error(
			`${packagePath} wasm:build must match the versioned WASM build contract`,
		);
	}
}

export function wasmPackBuildArgs(contract, outDir) {
	validateWasmBuildContract(contract);
	return contract.wasmPack.args.map((arg) =>
		arg === "{outDir}" ? outDir : arg,
	);
}
