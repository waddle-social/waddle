import { afterEach, describe, expect, test } from "bun:test";
import {
	mkdirSync,
	mkdtempSync,
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
	canonicalWasmBuildIdentity,
	collectWasmBuildInputs,
	wasmPackBuildArgs,
} from "../scripts/wasm-build-inputs.mjs";
import { renderWasmWrapper } from "../scripts/wasm-package-bindings.mjs";

const CONTRACT_PATH = "chat/scripts/wasm-build-contract.json";
const FORMAT = "waddle:xmpp-client-wasm:canonical-inputs:v1";
const roots: string[] = [];

const contract = {
	schemaVersion: 1,
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
		for (const directory of [
			".git",
			".jj",
			"node_modules",
			"target",
			"wasm-pkg",
		]) {
			write(
				root,
				`${sourceRoot}/${directory}/generated.rs`,
				"pub fn ignored() {}\n",
			);
		}
		expect(buildId(root)).toBe(before);
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

		const includeRoot = fixtureRoot();
		write(
			includeRoot,
			`${WASM_BUILD_INPUT_MANIFEST.sourceRoots[0]}/lib.rs`,
			'const DATA: &str = include_str!("data.txt");\n',
		);
		expect(() => buildId(includeRoot)).toThrow(
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
	});

	test("requires Bun, Cargo, rustc, and wasm-pack to resolve from Nix", () => {
		expect(() =>
			assertPinnedNixToolchain({
				bun: "/nix/store/bun/bin/bun",
				cargo: "/nix/store/rust/bin/cargo",
				rustc: "/nix/store/rust/bin/rustc",
				wasmPack: "/nix/store/wasm-pack/bin/wasm-pack",
			}),
		).not.toThrow();
		expect(() =>
			assertPinnedNixToolchain({
				bun: "/usr/local/bin/bun",
				cargo: "/nix/store/rust/bin/cargo",
				rustc: "/nix/store/rust/bin/rustc",
				wasmPack: "/nix/store/wasm-pack/bin/wasm-pack",
			}),
		).toThrow("flake-pinned /nix/store toolchain");
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
