import { afterEach, describe, expect, test } from "bun:test";
import {
	chmodSync,
	mkdirSync,
	mkdtempSync,
	realpathSync,
	readdirSync,
	renameSync,
	rmSync,
	symlinkSync,
	unlinkSync,
	utimesSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import {
	canonicalRelativePath,
	digestCanonicalInputs,
} from "../scripts/wasm-build-input-digest.mjs";
import {
	WASM_BUILD_INPUT_MANIFEST,
	assertHermeticWasmBuildEnvironment,
	assertPinnedNixToolchain,
	assertPinnedToolVersions,
	canonicalWasmBuildIdentity,
	collectWasmBuildInputs,
	resolveExecutableOnPath,
	wasmPackBuildArgs,
} from "../scripts/wasm-build-inputs.mjs";
import {
	PINNED_WASM_EXECUTOR_PROTOCOL,
	WASM_PACKAGE_ARTIFACTS,
	assertPinnedWasmBuildFilesystem,
	assertWasmArtifactSetsEqual,
	canonicalEncodedRustFlags,
	createIsolatedWasmBuildPaths,
	pinnedFlakeBuildArgs,
} from "../scripts/wasm-build-executor.mjs";
import { renderWasmWrapper } from "../scripts/wasm-package-bindings.mjs";

const CONTRACT_PATH = "chat/scripts/wasm-build-contract.json";
const BUILD_SCRIPT_PATH = "chat/scripts/build-xmpp-wasm.mjs";
const FORMAT = "waddle:xmpp-client-wasm:canonical-inputs:v2";
const roots: string[] = [];

const contract = {
	schemaVersion: 2,
	digestFormat: FORMAT,
	packageScript: "bun run scripts/build-xmpp-wasm.mjs",
	crate: "server/crates/waddle-xmpp-client-wasm",
	cargo: {
		profile: "release",
		target: "wasm32-unknown-unknown",
		defaultFeatures: true,
		features: [],
		locked: true,
	},
	wasmPack: {
		command: "wasm-pack",
		args: [
			"build",
			"--target",
			"bundler",
			"--release",
			"--out-dir",
			"{outDir}",
			"--",
			"--locked",
		],
	},
	executor: {
		protocol: PINNED_WASM_EXECUTOR_PROTOCOL,
		flake: "path:.",
		ignoreEnvironment: true,
		cleanCargoHome: true,
		cleanCargoTarget: true,
		artifactCount: 6,
		remapPathPrefixes: {
			buildRoot: "/waddle-build",
			repoRoot: "/waddle",
		},
	},
};

afterEach(() => {
	for (const root of roots.splice(0)) {
		rmSync(root, { recursive: true, force: true });
	}
});

function write(
	root: string,
	relativePath: string,
	contents: string | Uint8Array,
) {
	const path = resolve(root, ...relativePath.split("/"));
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(path, contents);
}

function fixtureRoot() {
	const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-inputs-"));
	roots.push(root);
	for (const path of WASM_BUILD_INPUT_MANIFEST.requiredFiles) {
		write(
			root,
			path,
			path === CONTRACT_PATH
				? `${JSON.stringify(contract, null, 2)}\n`
				: path.endsWith(".mjs")
					? `// fixture:${path}\n`
					: `fixture:${path}\n`,
		);
	}
	write(
		root,
		"chat/package.json",
		`${JSON.stringify({ scripts: { "wasm:build": contract.packageScript } }, null, 2)}\n`,
	);
	for (const sourceRoot of WASM_BUILD_INPUT_MANIFEST.sourceRoots) {
		write(root, `${sourceRoot}/lib.rs`, "pub fn fixture() {}\n");
		write(root, `${sourceRoot}/zeta.rs`, "pub fn zeta() {}\n");
		write(root, `${sourceRoot}/alpha.rs`, "pub fn alpha() {}\n");
	}
	return root;
}

function buildId(root: string) {
	return canonicalWasmBuildIdentity(root).buildId;
}

describe("canonical WASM build identity", () => {
	test("is independent of the absolute checkout root", () => {
		expect(buildId(fixtureRoot())).toBe(buildId(fixtureRoot()));
	});

	test("is independent of input enumeration order", () => {
		const { contract: loaded, entries } = collectWasmBuildInputs(fixtureRoot());
		expect(digestCanonicalInputs(entries, loaded.digestFormat)).toBe(
			digestCanonicalInputs(entries.toReversed(), loaded.digestFormat),
		);
	});

	test("uses framed path and content bytes rather than ambiguous concatenation", () => {
		const left = digestCanonicalInputs(
			[
				{ path: "a", bytes: Buffer.from("bc") },
				{ path: "def", bytes: Buffer.from("") },
			],
			FORMAT,
		);
		const right = digestCanonicalInputs(
			[
				{ path: "ab", bytes: Buffer.from("c") },
				{ path: "def", bytes: Buffer.from("") },
			],
			FORMAT,
		);
		expect(left).not.toBe(right);
		expect(left).toHaveLength(64);
	});

	test("changes when an input path or content changes", () => {
		const contentRoot = fixtureRoot();
		const beforeContent = buildId(contentRoot);
		write(contentRoot, "flake.lock", "different locked toolchain\n");
		expect(buildId(contentRoot)).not.toBe(beforeContent);

		const configRoot = fixtureRoot();
		const tomlRoot = fixtureRoot();
		write(configRoot, "server/.cargo/config", "[build]\n");
		write(tomlRoot, "server/.cargo/config.toml", "[build]\n");
		expect(buildId(configRoot)).not.toBe(buildId(tomlRoot));
	});

	test("changes with the descriptor and pinned toolchain inputs", () => {
		const descriptorRoot = fixtureRoot();
		const beforeDescriptor = buildId(descriptorRoot);
		write(
			descriptorRoot,
			CONTRACT_PATH,
			`${JSON.stringify({ ...contract, digestFormat: `${FORMAT}-revision` }, null, 2)}\n`,
		);
		expect(buildId(descriptorRoot)).not.toBe(beforeDescriptor);

		const toolchainRoot = fixtureRoot();
		const beforeToolchain = buildId(toolchainRoot);
		write(
			toolchainRoot,
			"server/rust-toolchain.toml",
			'[toolchain]\nchannel = "new"\n',
		);
		expect(buildId(toolchainRoot)).not.toBe(beforeToolchain);
	});

	test("ignores mtimes, generated outputs, and explicitly classified temporary files", () => {
		const root = fixtureRoot();
		const before = buildId(root);
		const flake = resolve(root, "flake.nix");
		utimesSync(flake, new Date(1_000), new Date(2_000));
		write(
			root,
			"server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm_bg.wasm",
			"host bytes",
		);
		write(
			root,
			"server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm_bg.js",
			"host glue",
		);
		const sourceRoot = WASM_BUILD_INPUT_MANIFEST.sourceRoots[0];
		write(root, `${sourceRoot}/scratch.tmp`, "temporary");
		write(root, `${sourceRoot}/editor.rs~`, "temporary");
		expect(buildId(root)).toBe(before);
	});

	test("includes every nested Rust source directory and fails closed on nested non-Rust files", () => {
		const nestedRoot = fixtureRoot();
		const sourceRoot = WASM_BUILD_INPUT_MANIFEST.sourceRoots[0];
		const before = buildId(nestedRoot);
		write(
			nestedRoot,
			`${sourceRoot}/target/generated.rs`,
			"pub fn nested_source() {}\n",
		);
		expect(buildId(nestedRoot)).not.toBe(before);

		const unsupportedRoot = fixtureRoot();
		write(unsupportedRoot, `${sourceRoot}/node_modules/schema.json`, "{}\n");
		expect(() => buildId(unsupportedRoot)).toThrow(
			"unsupported WASM build source input",
		);
	});

	test("fails closed for missing required files and crate entry points", () => {
		const requiredRoot = fixtureRoot();
		unlinkSync(resolve(requiredRoot, "server/Cargo.lock"));
		expect(() => buildId(requiredRoot)).toThrow(
			"missing required WASM build input",
		);

		const sourceRoot = fixtureRoot();
		unlinkSync(
			resolve(sourceRoot, WASM_BUILD_INPUT_MANIFEST.sourceRoots[1], "lib.rs"),
		);
		expect(() => buildId(sourceRoot)).toThrow(
			"missing required WASM crate entry point",
		);
	});

	test("fails closed for unexpected source candidates and compile-time includes", () => {
		const unexpectedRoot = fixtureRoot();
		write(
			unexpectedRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/schema.json`,
			"{}\n",
		);
		expect(() => buildId(unexpectedRoot)).toThrow(
			"unsupported WASM build source input",
		);

		for (const source of [
			'const DATA: &str = include_str!("data.txt");\n',
			'const DATA: &[u8] = include_bytes ! ["data.bin"];\n',
			'include! {"generated.rs"}\n',
			'const DATA: &[u8] = include_bytes /* outer /* nested */ comment */ ! ["data.bin"];\n',
			'const DATA: &str = include_str // comment\n ! ("data.txt");\n',
			'include! /* outer /* nested */ comment */ {"generated.rs"}\n',
		]) {
			const includeRoot = fixtureRoot();
			write(
				includeRoot,
				`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
				source,
			);
			expect(() => buildId(includeRoot)).toThrow(
				"unsupported compile-time include macro",
			);
		}

		const literalRoot = fixtureRoot();
		write(
			literalRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'const TEXT: &str = "include!(\\\"generated.rs\\\")";\nconst RAW: &str = r#"include_bytes![\\"data.bin\\"]"#;\n// include_str!("ignored.txt")\npub fn borrow<\'a>(value: &\'a str) -> &\'a str { value }\n',
		);
		expect(() => buildId(literalRoot)).not.toThrow();
	});

	test("rejects aliased compile-time include macros", () => {
		const aliasRoot = fixtureRoot();
		write(
			aliasRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'use std::{include as items, include_bytes as bytes, include_str as text};\nitems!("generated.rs");\nconst BYTES: &[u8] = bytes!("data.bin");\nconst TEXT: &str = text!("data.txt");\n',
		);
		expect(() => buildId(aliasRoot)).toThrow(
			"unsupported compile-time include macro",
		);
	});

	test("allows only canonical in-closure Rust path overrides", () => {
		const inClosureRoot = fixtureRoot();
		write(
			inClosureRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'#[path = "alpha.rs"]\nmod alpha;\n',
		);
		expect(() => buildId(inClosureRoot)).not.toThrow();

		const traversalRoot = fixtureRoot();
		write(
			traversalRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'#[path = "../outside.rs"]\nmod outside;\n',
		);
		expect(() => buildId(traversalRoot)).toThrow("not canonical");

		const dynamicRoot = fixtureRoot();
		write(
			dynamicRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'#[path = concat!("alpha", ".rs")]\nmod alpha;\n',
		);
		expect(() => buildId(dynamicRoot)).toThrow(
			"unsupported dynamic Rust #[path] override",
		);

		const decoyRoot = fixtureRoot();
		write(
			decoyRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'// #[path = "alpha.rs"]\n#[path = concat!("alpha", ".rs")]\nmod alpha;\n',
		);
		expect(() => buildId(decoyRoot)).toThrow(
			"unsupported dynamic Rust #[path] override",
		);

		const conditionalRoot = fixtureRoot();
		write(
			conditionalRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'#[cfg_attr(feature = "alternate", path = "alpha.rs")]\nmod alpha;\n',
		);
		expect(() => buildId(conditionalRoot)).toThrow(
			"unsupported conditional Rust #[cfg_attr",
		);
	});

	test("allows only declared relative build modules and explicit built-ins", () => {
		const declaredRoot = fixtureRoot();
		write(
			declaredRoot,
			BUILD_SCRIPT_PATH,
			'import "node:fs";\nimport "./wasm-build-inputs.mjs";\n',
		);
		expect(() => buildId(declaredRoot)).not.toThrow();

		for (const source of [
			'import "./undeclared-helper.mjs";\n',
			'await import("./undeclared-helper.mjs");\n',
			'const name = "helper";\nawait import(`./${name}.mjs`);\n',
			'import "external-package";\n',
			'import "./wasm-build-inputs";\n',
			'import "./wasm-build-inputs.mjs?raw";\n',
			'import "../../../../outside.mjs";\n',
		]) {
			const invalidRoot = fixtureRoot();
			write(invalidRoot, BUILD_SCRIPT_PATH, source);
			expect(() => buildId(invalidRoot)).toThrow();
		}
	});

	test("rejects symlinks, symlink ancestors, traversal, backslashes, and duplicates", () => {
		const fileLinkRoot = fixtureRoot();
		const sourceRoot = WASM_BUILD_INPUT_MANIFEST.sourceRoots[0];
		symlinkSync("lib.rs", resolve(fileLinkRoot, sourceRoot, "linked.rs"));
		expect(() => buildId(fileLinkRoot)).toThrow("must not be a symbolic link");

		const ancestorRoot = fixtureRoot();
		const sourcePath = resolve(ancestorRoot, sourceRoot);
		const realSourcePath = resolve(ancestorRoot, "real-source");
		renameSync(sourcePath, realSourcePath);
		symlinkSync(realSourcePath, sourcePath, "dir");
		expect(() => buildId(ancestorRoot)).toThrow(
			"input ancestor must be a real directory",
		);

		expect(() => canonicalRelativePath("../escape")).toThrow("not canonical");
		expect(() => canonicalRelativePath("server\\Cargo.toml")).toThrow(
			"must use '/'",
		);
		expect(() =>
			digestCanonicalInputs(
				[
					{ path: "same", bytes: Buffer.from("one") },
					{ path: "same", bytes: Buffer.from("two") },
				],
				FORMAT,
			),
		).toThrow("duplicate WASM build input path");
	});

	test("validates the exact locked invocation and the relevant package-script contract", () => {
		const root = fixtureRoot();
		const { contract: loaded } = collectWasmBuildInputs(root);
		expect(wasmPackBuildArgs(loaded, "/tmp/isolated-output")).toEqual([
			"build",
			"--target",
			"bundler",
			"--release",
			"--out-dir",
			"/tmp/isolated-output",
			"--",
			"--locked",
		]);

		write(
			root,
			"chat/package.json",
			`${JSON.stringify({ scripts: { "wasm:build": "different" } })}\n`,
		);
		expect(() => buildId(root)).toThrow(
			"must match the versioned WASM build contract",
		);
	});

	test("rejects any invocation, profile, target, or feature drift", () => {
		const invalidContracts = [
			{
				...contract,
				wasmPack: {
					...contract.wasmPack,
					args: [...contract.wasmPack.args, "--verbose"],
				},
			},
			{
				...contract,
				wasmPack: {
					...contract.wasmPack,
					args: [
						"build",
						"--release",
						"--target",
						"bundler",
						"--out-dir",
						"{outDir}",
						"--",
						"--locked",
					],
				},
			},
			{
				...contract,
				wasmPack: {
					...contract.wasmPack,
					args: [...contract.wasmPack.args, "--locked"],
				},
			},
			{
				...contract,
				wasmPack: {
					...contract.wasmPack,
					args: contract.wasmPack.args.filter(
						(argument) => argument !== "--locked",
					),
				},
			},
			{ ...contract, cargo: { ...contract.cargo, profile: "dev" } },
			{
				...contract,
				cargo: { ...contract.cargo, target: "wasm32-wasip1" },
			},
			{
				...contract,
				cargo: { ...contract.cargo, defaultFeatures: false },
			},
			{
				...contract,
				cargo: { ...contract.cargo, features: ["alternate"] },
			},
			{
				...contract,
				executor: { ...contract.executor, protocol: "unversioned" },
			},
			{
				...contract,
				executor: { ...contract.executor, ignoreEnvironment: false },
			},
			{
				...contract,
				executor: { ...contract.executor, artifactCount: 3 },
			},
			{
				...contract,
				executor: {
					...contract.executor,
					remapPathPrefixes: {
						...contract.executor.remapPathPrefixes,
						buildRoot: "/ambient-build",
					},
				},
			},
		];

		for (const invalidContract of invalidContracts) {
			const invalidRoot = fixtureRoot();
			write(
				invalidRoot,
				CONTRACT_PATH,
				`${JSON.stringify(invalidContract, null, 2)}\n`,
			);
			expect(() => buildId(invalidRoot)).toThrow("unsupported");
		}
	});
});

describe("canonical WASM build execution policy", () => {
	test("rejects output-affecting ambient Rust and Cargo configuration", () => {
		expect(() =>
			assertHermeticWasmBuildEnvironment({ CARGO_TARGET_DIR: "/tmp/isolated" }),
		).not.toThrow();
		for (const name of [
			"RUSTFLAGS",
			"RUSTC_WRAPPER",
			"CARGO_BUILD_TARGET",
			"CARGO_PROFILE_RELEASE_LTO",
			"CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER",
		]) {
			expect(() =>
				assertHermeticWasmBuildEnvironment({ [name]: "override" }),
			).toThrow("output-affecting Rust/Cargo environment");
		}

		const cargoHome = mkdtempSync(resolve(tmpdir(), "waddle-wasm-cargo-home-"));
		roots.push(cargoHome);
		writeFileSync(resolve(cargoHome, "config.toml"), "[build]\nrustflags = ['-Ctarget-cpu=native']\n");
		expect(() =>
			assertHermeticWasmBuildEnvironment({ CARGO_HOME: cargoHome }),
		).toThrow("ambient CARGO_HOME configuration");
	});

	test("requires exact flake tool paths and versions, not arbitrary Nix tools", () => {
		const expected = {
			bun: "/nix/store/flake-bun/bin/bun",
			cargo: "/nix/store/flake-rust/bin/cargo",
			rustc: "/nix/store/flake-rust/bin/rustc",
			wasmPack: "/nix/store/flake-wasm-pack/bin/wasm-pack",
			wasmBindgen: "/nix/store/flake-wasm-bindgen/bin/wasm-bindgen",
		};
		expect(() =>
			assertPinnedNixToolchain(expected, expected),
		).not.toThrow();
		expect(() =>
			assertPinnedNixToolchain(
				{ ...expected, bun: "/nix/store/ambient-bun/bin/bun" },
				expected,
			),
		).toThrow("exact repository-flake tool");

		const versions = {
			bun: "1.3.0",
			cargo: "1.88.0",
			rustc: "1.88.0",
			wasmPack: "0.13.1",
			wasmBindgen: "0.2.120",
		};
		expect(() => assertPinnedToolVersions(versions, versions)).not.toThrow();
		expect(() =>
			assertPinnedToolVersions(
				{ ...versions, wasmPack: "0.13.0" },
				versions,
			),
		).toThrow("version must match the repository flake");
	});

	test("resolves tools directly from PATH without depending on which", () => {
		const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-path-"));
		roots.push(root);
		const bin = resolve(root, "bin");
		mkdirSync(bin);
		const cargo = resolve(bin, "cargo");
		writeFileSync(cargo, "#!/bin/sh\nexit 0\n");
		chmodSync(cargo, 0o755);
		expect(resolveExecutableOnPath("cargo", bin)).toBe(realpathSync(cargo));
		expect(() => resolveExecutableOnPath("which", bin)).toThrow(
			"could not resolve which",
		);
	});

	test("allocates fresh config-free homes and rejects target contamination", () => {
		const runRoot = mkdtempSync(resolve(tmpdir(), "waddle-wasm-run-test-"));
		roots.push(runRoot);
		const first = createIsolatedWasmBuildPaths(runRoot, "first");
		const second = createIsolatedWasmBuildPaths(runRoot, "second");
		expect(first.root).not.toBe(second.root);
		expect(first.cargoHome).not.toBe(second.cargoHome);
		expect(first.cargoTarget).not.toBe(second.cargoTarget);
		expect(readdirSync(second.cargoHome)).toEqual([]);
		expect(readdirSync(second.cargoTarget)).toEqual([]);

		const environment = {
			WADDLE_WASM_BUILD_ROOT: first.root,
			CARGO_HOME: first.cargoHome,
			CARGO_TARGET_DIR: first.cargoTarget,
			HOME: first.home,
		};
		expect(() =>
			assertPinnedWasmBuildFilesystem(environment, first.outDir),
		).not.toThrow();
		writeFileSync(resolve(first.cargoTarget, "ambient-artifact"), "poison");
		expect(() =>
			assertPinnedWasmBuildFilesystem(environment, first.outDir),
		).toThrow("must start empty");
	});

	test("renders a locked path-flake command without VCS discovery", () => {
		const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-no-vcs-"));
		roots.push(root);
		const repoRoot = resolve(root, "checkout-without-dot-git");
		mkdirSync(repoRoot);
		const paths = createIsolatedWasmBuildPaths(root, "build");
		const args = pinnedFlakeBuildArgs({
			repoRoot,
			scriptPath: resolve(repoRoot, BUILD_SCRIPT_PATH),
			outDir: paths.outDir,
			paths,
			executor: contract.executor,
		});
		expect(args.slice(0, 5)).toEqual([
			"develop",
			"--no-update-lock-file",
			"--no-write-lock-file",
			"--ignore-environment",
			`path:${repoRoot}`,
		]);
		expect(args).not.toContain("git");
		expect(args).toContain(`CARGO_HOME=${paths.cargoHome}`);
		expect(args).toContain(`CARGO_TARGET_DIR=${paths.cargoTarget}`);
		expect(args).toContain(
			`CARGO_ENCODED_RUSTFLAGS=${canonicalEncodedRustFlags(
				repoRoot,
				paths.root,
				contract.executor.remapPathPrefixes,
			)}`,
		);
	});

	test("compares all six compiled package artifacts byte-for-byte", () => {
		const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-artifacts-"));
		roots.push(root);
		const left = resolve(root, "left");
		const right = resolve(root, "right");
		mkdirSync(left);
		mkdirSync(right);
		for (const artifact of WASM_PACKAGE_ARTIFACTS) {
			writeFileSync(resolve(left, artifact), `same:${artifact}`);
			writeFileSync(resolve(right, artifact), `same:${artifact}`);
		}
		expect(() => assertWasmArtifactSetsEqual(left, right)).not.toThrow();
		writeFileSync(
			resolve(right, "waddle_xmpp_client_wasm_bg.wasm"),
			"different compiled bytes",
		);
		expect(() => assertWasmArtifactSetsEqual(left, right)).toThrow(
			"waddle_xmpp_client_wasm_bg.wasm",
		);
	});

	test("renders all wrapper cache-bust references from one full digest", () => {
		const id = "a".repeat(64);
		const wrapper = renderWasmWrapper(id);
		const glueIds = [
			...wrapper.matchAll(/waddle_xmpp_client_wasm_bg\.js\?b=([0-9a-f]+)/gu),
		].map((match) => match[1]);
		expect(glueIds).toEqual([id, id, id]);
		expect(wrapper).toContain(`waddle_xmpp_client_wasm_bg.wasm?url&b=${id}`);
		expect(wrapper.match(new RegExp(id, "gu"))).toHaveLength(4);
	});
});
