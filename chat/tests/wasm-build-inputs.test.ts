import { afterEach, describe, expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import {
	chmodSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
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
import { gunzipSync } from "node:zlib";
import {
	GITHUB_PACKAGES_REGISTRY,
	buildAndPublishWasm,
} from "../scripts/build-and-publish-wasm.mjs";
import {
	PINNED_CARGO_METADATA_PROTOCOL,
	pinnedCargoMetadataArgs,
} from "../scripts/wasm-cargo-metadata-executor.mjs";
import {
	canonicalRelativePath,
	digestCanonicalInputs,
} from "../scripts/wasm-build-input-digest.mjs";
import {
	WASM_BUILD_INPUT_MANIFEST,
	REPOSITORY_CARGO_CONFIG_PATHS,
	assertHermeticWasmBuildEnvironment,
	assertNoAmbientCargoAncestorConfig,
	assertNoRepositoryCargoConfig,
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
	assertCanonicalWasmBuildId,
	assertPinnedWasmBuildFilesystem,
	assertWasmArtifactSetsEqual,
	assertWasmBuildIdentityHandoff,
	canonicalEncodedRustFlags,
	createIsolatedWasmBuildPaths,
	findDriftedWasmArtifacts,
	pinnedFlakeBuildArgs,
} from "../scripts/wasm-build-executor.mjs";
import {
	finalizeWasmPackage,
	renderWasmWrapper,
	wasmPackageVersion,
} from "../scripts/wasm-package-bindings.mjs";

const CONTRACT_PATH = "chat/scripts/wasm-build-contract.json";
const BUILD_SCRIPT_PATH = "chat/scripts/build-xmpp-wasm.mjs";
const FORMAT = "waddle:xmpp-client-wasm:canonical-inputs:v3";
const REAL_REPO_ROOT = resolve(import.meta.dir, "../..");
const roots: string[] = [];
const MINIMAL_RAW_WASM_PACK_DECLARATIONS = `export type WaddleResumeStanzaKind = "message" | "presence" | "iq";
export interface WaddleResumeXmlName {
    readonly namespace: string;
    readonly localName: string;
}
export interface WaddleResumeXmlAttribute {
    readonly name: WaddleResumeXmlName;
    readonly value: string;
}
export type WaddleResumeXmlToken =
| {
    readonly kind: "start";
    readonly name: WaddleResumeXmlName;
    readonly attributes: WaddleResumeXmlAttribute[];
}
| { readonly kind: "text"; readonly value: string }
| { readonly kind: "end" };
export interface WaddleResumeStanzaSnapshot {
    readonly stanzaKind: WaddleResumeStanzaKind;
    readonly tokens: WaddleResumeXmlToken[];
}
export interface WaddleResumeEntrySnapshot {
    readonly stanza: WaddleResumeStanzaSnapshot;
    readonly sentAtEpochMs: number;
}
export interface WaddleResumeStateSnapshot {
    readonly previd: string;
    readonly inboundH: number;
    readonly outboundH: number;
    readonly unhandledOutboundEntries: WaddleResumeEntrySnapshot[];
    readonly maxResumeSeconds?: number;
}
export class WaddleClient {
    get_resume_state(): any;
    with_resume_state(state: any): void;
    send_chat_message(peer_jid: string, body: string, options: any): Promise<any>;
    send_groupchat_message(room_jid: string, body: string, options: any): Promise<any>;
    set_on_call(cb: Function): void;
    set_on_connected(cb: Function): void;
    set_on_disconnected(cb: Function): void;
    set_on_error(cb: Function): void;
    set_on_mds_displayed(cb: Function): void;
    set_on_message(cb: Function): void;
    set_on_message_delivery_acked(cb: Function): void;
    set_on_message_delivery_failed(cb: Function): void;
    set_on_presence(cb: Function): void;
    set_on_pubsub_event(cb: Function): void;
    set_on_session_lifecycle(cb: Function): void;
    set_on_stream_management(cb: Function): void;
}
`;
const cargoMetadataByRoot = new Map<
	string,
	ReturnType<typeof baseCargoMetadata>
>();
const BASE_CRATES = Object.freeze([
	{
		name: "waddle-xmpp-client-wasm",
		path: "server/crates/waddle-xmpp-client-wasm",
		kind: ["cdylib", "rlib"],
	},
	{
		name: "waddle-xmpp-client",
		path: "server/crates/waddle-xmpp-client",
		kind: ["lib"],
	},
	{
		name: "waddle-xmpp-core",
		path: "server/crates/waddle-xmpp-core",
		kind: ["lib"],
	},
]);
const FIRST_SOURCE_ROOT = `${BASE_CRATES[0].path}/src`;
const SECOND_SOURCE_ROOT = `${BASE_CRATES[1].path}/src`;

const contract = {
	schemaVersion: 3,
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
	cargoMetadataByRoot.clear();
});

const TAR_BLOCK_BYTES = 512;

function readTarField(
	archive: Uint8Array,
	offset: number,
	length: number,
): string {
	const field = archive.subarray(offset, offset + length);
	const terminator = field.indexOf(0);
	return Buffer.from(
		terminator === -1 ? field : field.subarray(0, terminator),
	).toString("utf8");
}

function readTarSize(archive: Uint8Array, headerOffset: number): number {
	const encoded = readTarField(archive, headerOffset + 124, 12).trim();
	if (!/^[0-7]+$/u.test(encoded)) {
		throw new Error(`packed archive has an invalid tar size: ${encoded}`);
	}
	const size = Number.parseInt(encoded, 8);
	if (!Number.isSafeInteger(size)) {
		throw new Error(`packed archive has an unsafe tar size: ${encoded}`);
	}
	return size;
}

function packedArtifactNames(archivePath: string): string[] {
	const archive = gunzipSync(readFileSync(archivePath));
	const entries: string[] = [];
	let offset = 0;

	while (offset + TAR_BLOCK_BYTES <= archive.length) {
		const header = archive.subarray(offset, offset + TAR_BLOCK_BYTES);
		if (header.every((byte) => byte === 0)) {
			break;
		}

		const name = readTarField(archive, offset, 100);
		const prefix = readTarField(archive, offset + 345, 155);
		const entryPath = prefix === "" ? name : `${prefix}/${name}`;
		const type = header[156];
		const size = readTarSize(archive, offset);
		if (type === 0 || type === "0".charCodeAt(0)) {
			entries.push(entryPath);
		} else if (type === "5".charCodeAt(0)) {
			if (size !== 0) {
				throw new Error(`packed archive directory has a payload: ${entryPath}`);
			}
		} else {
			throw new Error(
				`packed archive has an unsupported tar entry: ${entryPath} (${String.fromCharCode(type ?? 0)})`,
			);
		}

		const paddedSize = Math.ceil(size / TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES;
		offset += TAR_BLOCK_BYTES + paddedSize;
		if (offset > archive.length) {
			throw new Error(
				`packed archive entry exceeds its tar payload: ${entryPath}`,
			);
		}
	}

	return entries.map((entry) => {
		const packagePrefix = "package/";
		if (!entry.startsWith(packagePrefix)) {
			throw new Error(`packed archive entry is outside package/: ${entry}`);
		}
		return entry.slice(packagePrefix.length);
	});
}

function write(
	root: string,
	relativePath: string,
	contents: string | Uint8Array,
) {
	const path = resolve(root, ...relativePath.split("/"));
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(path, contents);
}

function packageId(root: string, cratePath: string, name: string) {
	return `path+file://${resolve(root, cratePath)}#${name}@0.1.0`;
}

function localPackage(
	root: string,
	{
		name,
		path,
		kind = ["lib"],
		source = `${path}/src/lib.rs`,
		buildScript,
	}: {
		name: string;
		path: string;
		kind?: string[];
		source?: string;
		buildScript?: string;
	},
) {
	return {
		id: packageId(root, path, name),
		name,
		version: "0.1.0",
		source: null,
		manifest_path: resolve(root, path, "Cargo.toml"),
		targets: [
			{
				kind,
				crate_types: kind,
				src_path: resolve(root, source),
			},
			...(buildScript
				? [
						{
							kind: ["custom-build"],
							crate_types: ["bin"],
							src_path: resolve(root, buildScript),
						},
					]
				: []),
		],
	};
}

function normalDependency(pkg: string) {
	return { pkg, dep_kinds: [{ kind: null, target: null }] };
}

function baseCargoMetadata(root: string) {
	const packages = BASE_CRATES.map((definition) =>
		localPackage(root, definition),
	);
	const [wasm, client, core] = packages;
	return {
		version: 1,
		packages,
		resolve: {
			root: null,
			nodes: [
				{
					id: wasm.id,
					deps: [normalDependency(client.id), normalDependency(core.id)],
				},
				{ id: client.id, deps: [normalDependency(core.id)] },
				{ id: core.id, deps: [] },
			],
		},
	};
}

function addResolvedLocalPackage(
	root: string,
	definition: Parameters<typeof localPackage>[1],
) {
	const metadata = cargoMetadataByRoot.get(root);
	if (!metadata) throw new Error("fixture Cargo metadata missing");
	const pkg = localPackage(root, definition);
	metadata.packages.push(pkg);
	metadata.resolve.nodes.push({ id: pkg.id, deps: [] });
	metadata.resolve.nodes[0].deps.push(normalDependency(pkg.id));
	write(
		root,
		`${definition.path}/Cargo.toml`,
		`[package]\nname = "${definition.name}"\n`,
	);
	write(
		root,
		definition.source ?? `${definition.path}/src/lib.rs`,
		"pub fn fixture_dependency() {}\n",
	);
	if (definition.buildScript) {
		write(root, definition.buildScript, "fn main() {}\n");
	}
	return { metadata, pkg };
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
	for (const crate of BASE_CRATES) {
		write(
			root,
			`${crate.path}/Cargo.toml`,
			`[package]\nname = "${crate.name}"\n`,
		);
		write(root, `${crate.path}/src/lib.rs`, "pub fn fixture() {}\n");
		write(root, `${crate.path}/src/zeta.rs`, "pub fn zeta() {}\n");
		write(root, `${crate.path}/src/alpha.rs`, "pub fn alpha() {}\n");
	}
	cargoMetadataByRoot.set(root, baseCargoMetadata(root));
	return root;
}

function buildId(root: string) {
	const cargoMetadata = cargoMetadataByRoot.get(root);
	if (!cargoMetadata) throw new Error("fixture Cargo metadata missing");
	return canonicalWasmBuildIdentity(root, { cargoMetadata }).buildId;
}

describe("canonical WASM build identity", () => {
	test("is independent of the absolute checkout root", () => {
		expect(buildId(fixtureRoot())).toBe(buildId(fixtureRoot()));
	});

	test("is independent of input enumeration order", () => {
		const root = fixtureRoot();
		const { contract: loaded, entries } = collectWasmBuildInputs(root, {
			cargoMetadata: cargoMetadataByRoot.get(root),
		});
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
	});

	test("rejects all eight in-repository Cargo config locations before metadata or build execution", () => {
		const configContents = [
			"[alias]\nwasm = 'build --release'\n",
			"[build]\nrustc-wrapper = '/tmp/wrapper'\n",
			"[target.wasm32-unknown-unknown]\nlinker = '/tmp/linker'\n",
			"[target.wasm32-unknown-unknown]\nrustflags = ['-Ctarget-feature=+simd128']\n",
			"[source.crates-io]\nreplace-with = 'vendored'\n",
			"[source.vendored]\ndirectory = '/tmp/vendor'\n",
			"[patch.crates-io]\nfixture = { path = '/tmp/fixture' }\n",
			"[build]\ntarget = 'wasm32-unknown-unknown'\n",
		];
		expect(REPOSITORY_CARGO_CONFIG_PATHS).toHaveLength(8);
		for (const [index, configPath] of REPOSITORY_CARGO_CONFIG_PATHS.entries()) {
			const configRoot = fixtureRoot();
			write(configRoot, configPath, configContents[index]);
			let metadataCalls = 0;
			expect(() =>
				canonicalWasmBuildIdentity(configRoot, {
					loadCargoMetadata: () => {
						metadataCalls += 1;
						return cargoMetadataByRoot.get(configRoot);
					},
				}),
			).toThrow(
				`repository Cargo configuration is not allowed for the canonical WASM build: ${configPath}`,
			);
			expect(metadataCalls).toBe(0);
			expect(() => assertNoRepositoryCargoConfig(configRoot)).toThrow(
				configPath,
			);
		}

		const brokenLinkRoot = fixtureRoot();
		const brokenConfig = resolve(brokenLinkRoot, ".cargo/config.toml");
		mkdirSync(dirname(brokenConfig), { recursive: true });
		symlinkSync("missing-config.toml", brokenConfig);
		expect(() => assertNoRepositoryCargoConfig(brokenLinkRoot)).toThrow(
			".cargo/config.toml",
		);
	});

	test("discovers every resolved local path dependency from Cargo metadata", () => {
		const root = fixtureRoot();
		const before = buildId(root);
		addResolvedLocalPackage(root, {
			name: "waddle-local-dependency",
			path: "server/crates/waddle-local-dependency",
		});
		expect(buildId(root)).not.toBe(before);
	});

	test("hashes custom library targets", () => {
		const root = fixtureRoot();
		addResolvedLocalPackage(root, {
			name: "waddle-custom-target",
			path: "server/crates/waddle-custom-target",
			source: "server/crates/waddle-custom-target/code/entry.rs",
		});
		const beforeSource = buildId(root);
		write(
			root,
			"server/crates/waddle-custom-target/code/entry.rs",
			"pub fn changed_target() {}\n",
		);
		expect(buildId(root)).not.toBe(beforeSource);
	});

	test("rejects every reachable local custom-build target", () => {
		const root = fixtureRoot();
		addResolvedLocalPackage(root, {
			name: "waddle-custom-build",
			path: "server/crates/waddle-custom-build",
			buildScript: "server/crates/waddle-custom-build/build.rs",
		});
		expect(() => buildId(root)).toThrow(
			"custom-build targets are unsupported until their complete filesystem input boundary is declared",
		);
	});

	test("rejects local package, target, build-script, and source-identity escapes", () => {
		const packageEscape = fixtureRoot();
		addResolvedLocalPackage(packageEscape, {
			name: "outside-trigger-boundary",
			path: "server/extensions/outside-trigger-boundary",
		});
		expect(() => buildId(packageEscape)).toThrow(
			"local Cargo package must live under server/crates",
		);

		const targetEscape = fixtureRoot();
		addResolvedLocalPackage(targetEscape, {
			name: "target-escape",
			path: "server/crates/target-escape",
			source: "server/crates/escaped-target.rs",
		});
		expect(() => buildId(targetEscape)).toThrow(
			"Cargo target for server/crates/target-escape/Cargo.toml escapes its Cargo package",
		);

		const buildEscape = fixtureRoot();
		addResolvedLocalPackage(buildEscape, {
			name: "build-escape",
			path: "server/crates/build-escape",
			buildScript: "server/crates/escaped-build.rs",
		});
		expect(() => buildId(buildEscape)).toThrow(
			"Cargo target for server/crates/build-escape/Cargo.toml escapes its Cargo package",
		);

		const sourceEscape = fixtureRoot();
		const metadata = cargoMetadataByRoot.get(sourceEscape);
		if (!metadata) throw new Error("fixture Cargo metadata missing");
		metadata.packages[1].source = "ambient+unlocked";
		expect(() => buildId(sourceEscape)).toThrow(
			"unsupported Cargo package source identity",
		);
	});

	test("changes with the descriptor and pinned toolchain inputs", () => {
		const descriptorRoot = fixtureRoot();
		const beforeDescriptor = buildId(descriptorRoot);
		write(
			descriptorRoot,
			CONTRACT_PATH,
			`${JSON.stringify({ ...contract, attestationRevision: "changed" }, null, 2)}\n`,
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
		const sourceRoot = FIRST_SOURCE_ROOT;
		write(root, `${sourceRoot}/scratch.tmp`, "temporary");
		write(root, `${sourceRoot}/editor.rs~`, "temporary");
		expect(buildId(root)).toBe(before);
	});

	test("includes every nested Rust source directory and fails closed on nested non-Rust files", () => {
		const nestedRoot = fixtureRoot();
		const sourceRoot = FIRST_SOURCE_ROOT;
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
		unlinkSync(resolve(sourceRoot, SECOND_SOURCE_ROOT, "lib.rs"));
		expect(() => buildId(sourceRoot)).toThrow(
			"missing required WASM crate entry point",
		);
	});

	test("fails closed for unexpected source candidates and compile-time includes", () => {
		const unexpectedRoot = fixtureRoot();
		write(unexpectedRoot, `${FIRST_SOURCE_ROOT}/schema.json`, "{}\n");
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
			write(includeRoot, `${FIRST_SOURCE_ROOT}/lib.rs`, source);
			expect(() => buildId(includeRoot)).toThrow(
				"unsupported compile-time include macro",
			);
		}

		const literalRoot = fixtureRoot();
		write(
			literalRoot,
			`${FIRST_SOURCE_ROOT}/lib.rs`,
			'const TEXT: &str = "include!(\\\"generated.rs\\\")";\nconst RAW: &str = r#"include_bytes![\\"data.bin\\"]"#;\n// include_str!("ignored.txt")\npub fn borrow<\'a>(value: &\'a str) -> &\'a str { value }\n',
		);
		expect(() => buildId(literalRoot)).not.toThrow();
	});

	test("rejects aliased compile-time include macros", () => {
		const aliasRoot = fixtureRoot();
		write(
			aliasRoot,
			`${FIRST_SOURCE_ROOT}/lib.rs`,
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
			`${FIRST_SOURCE_ROOT}/lib.rs`,
			'#[path = "alpha.rs"]\nmod alpha;\n',
		);
		expect(() => buildId(inClosureRoot)).not.toThrow();

		const traversalRoot = fixtureRoot();
		write(
			traversalRoot,
			`${FIRST_SOURCE_ROOT}/lib.rs`,
			'#[path = "../outside.rs"]\nmod outside;\n',
		);
		expect(() => buildId(traversalRoot)).toThrow("not canonical");

		const dynamicRoot = fixtureRoot();
		write(
			dynamicRoot,
			`${FIRST_SOURCE_ROOT}/lib.rs`,
			'#[path = concat!("alpha", ".rs")]\nmod alpha;\n',
		);
		expect(() => buildId(dynamicRoot)).toThrow(
			"unsupported dynamic Rust #[path] override",
		);

		const decoyRoot = fixtureRoot();
		write(
			decoyRoot,
			`${FIRST_SOURCE_ROOT}/lib.rs`,
			'// #[path = "alpha.rs"]\n#[path = concat!("alpha", ".rs")]\nmod alpha;\n',
		);
		expect(() => buildId(decoyRoot)).toThrow(
			"unsupported dynamic Rust #[path] override",
		);

		const conditionalRoot = fixtureRoot();
		write(
			conditionalRoot,
			`${FIRST_SOURCE_ROOT}/lib.rs`,
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
		const sourceRoot = FIRST_SOURCE_ROOT;
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
		const { contract: loaded } = collectWasmBuildInputs(root, {
			cargoMetadata: cargoMetadataByRoot.get(root),
		});
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
				digestFormat: "waddle:xmpp-client-wasm:canonical-inputs:v2",
			},
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
		writeFileSync(
			resolve(cargoHome, "config.toml"),
			"[build]\nrustflags = ['-Ctarget-cpu=native']\n",
		);
		expect(() =>
			assertHermeticWasmBuildEnvironment({ CARGO_HOME: cargoHome }),
		).toThrow("ambient CARGO_HOME configuration");

		const ancestorRoot = mkdtempSync(
			resolve(tmpdir(), "waddle-wasm-cargo-ancestor-"),
		);
		roots.push(ancestorRoot);
		const repoRoot = resolve(ancestorRoot, "checkout");
		mkdirSync(repoRoot);
		write(ancestorRoot, ".cargo/config.toml", "[build]\nrustflags = []\n");
		expect(() => assertNoAmbientCargoAncestorConfig(repoRoot)).toThrow(
			"ambient Cargo ancestor configuration",
		);
	});

	test("requires exact flake tool paths and versions, not arbitrary Nix tools", () => {
		const expected = {
			bun: "/nix/store/flake-bun/bin/bun",
			cargo: "/nix/store/flake-rust/bin/cargo",
			rustc: "/nix/store/flake-rust/bin/rustc",
			wasmPack: "/nix/store/flake-wasm-pack/bin/wasm-pack",
			wasmBindgen: "/nix/store/flake-wasm-bindgen/bin/wasm-bindgen",
		};
		expect(() => assertPinnedNixToolchain(expected, expected)).not.toThrow();
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
			assertPinnedToolVersions({ ...versions, wasmPack: "0.13.0" }, versions),
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
		const buildId = "b".repeat(64);
		const args = pinnedFlakeBuildArgs({
			repoRoot,
			scriptPath: resolve(repoRoot, BUILD_SCRIPT_PATH),
			outDir: paths.outDir,
			paths,
			executor: contract.executor,
			buildId,
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
		expect(args).toContain(`WADDLE_WASM_BUILD_ID=${buildId}`);
		expect(args.slice(-3)).toEqual([
			"--internal-pinned-build",
			paths.outDir,
			buildId,
		]);
		expect(args).toContain(
			`CARGO_ENCODED_RUSTFLAGS=${canonicalEncodedRustFlags(
				repoRoot,
				paths.root,
				contract.executor.remapPathPrefixes,
			)}`,
		);
	});

	test("pins Cargo metadata and rejects invalid build-identity handoffs", () => {
		const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-metadata-test-"));
		roots.push(root);
		const repoRoot = resolve(root, "checkout");
		const runRoot = resolve(root, "run");
		const cargoHome = resolve(root, "cargo-home");
		const home = resolve(root, "home");
		const outputPath = resolve(runRoot, "metadata.json");
		const scriptPath = resolve(
			repoRoot,
			"chat/scripts/wasm-cargo-metadata.mjs",
		);
		const metadataArgs = pinnedCargoMetadataArgs({
			repoRoot,
			scriptPath,
			outputPath,
			runRoot,
			cargoHome,
			home,
		});
		expect(metadataArgs.slice(0, 5)).toEqual([
			"develop",
			"--no-update-lock-file",
			"--no-write-lock-file",
			"--ignore-environment",
			`path:${repoRoot}`,
		]);
		expect(metadataArgs).toContain(
			`WADDLE_WASM_METADATA_PROTOCOL=${PINNED_CARGO_METADATA_PROTOCOL}`,
		);
		expect(metadataArgs.slice(-3)).toEqual([
			scriptPath,
			"--internal-pinned-metadata",
			outputPath,
		]);

		const buildId = "c".repeat(64);
		expect(() => assertCanonicalWasmBuildId(undefined)).toThrow(
			"exactly 64 lowercase hex characters",
		);
		expect(() => assertCanonicalWasmBuildId("C".repeat(64))).toThrow(
			"exactly 64 lowercase hex characters",
		);
		expect(() => assertWasmBuildIdentityHandoff({}, buildId)).toThrow(
			"identity handoff does not match",
		);
		expect(() =>
			assertWasmBuildIdentityHandoff(
				{ WADDLE_WASM_BUILD_ID: "d".repeat(64) },
				buildId,
			),
		).toThrow("identity handoff does not match");
		expect(() =>
			assertWasmBuildIdentityHandoff(
				{ WADDLE_WASM_BUILD_ID: buildId },
				buildId,
			),
		).not.toThrow();
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
		expect(() =>
			assertWasmArtifactSetsEqual(left, right, contract.executor.artifactCount),
		).not.toThrow();
		writeFileSync(
			resolve(right, "waddle_xmpp_client_wasm_bg.wasm"),
			"different compiled bytes",
		);
		expect(() =>
			assertWasmArtifactSetsEqual(left, right, contract.executor.artifactCount),
		).toThrow("waddle_xmpp_client_wasm_bg.wasm");
	});

	test("committed drift comparison covers compiled bytes and missing canonical artifacts", () => {
		const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-committed-drift-"));
		roots.push(root);
		const committed = resolve(root, "committed");
		const canonical = resolve(root, "canonical");
		mkdirSync(committed);
		mkdirSync(canonical);
		for (const artifact of WASM_PACKAGE_ARTIFACTS) {
			writeFileSync(resolve(committed, artifact), `same:${artifact}`);
			writeFileSync(resolve(canonical, artifact), `same:${artifact}`);
		}

		expect(findDriftedWasmArtifacts(committed, canonical)).toEqual([]);
		writeFileSync(
			resolve(canonical, "waddle_xmpp_client_wasm_bg.wasm"),
			"different compiled bytes",
		);
		expect(findDriftedWasmArtifacts(committed, canonical)).toEqual([
			"waddle_xmpp_client_wasm_bg.wasm",
		]);

		writeFileSync(
			resolve(canonical, "waddle_xmpp_client_wasm_bg.wasm"),
			"same:waddle_xmpp_client_wasm_bg.wasm",
		);
		unlinkSync(resolve(committed, "waddle_xmpp_client_wasm_bg.js"));
		expect(findDriftedWasmArtifacts(committed, canonical)).toEqual([
			"waddle_xmpp_client_wasm_bg.js",
		]);
	});

	test("rejects noncanonical build-output trees before byte comparison", () => {
		function artifactPair() {
			const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-artifact-tree-"));
			roots.push(root);
			const left = resolve(root, "left");
			const right = resolve(root, "right");
			mkdirSync(left);
			mkdirSync(right);
			for (const artifact of WASM_PACKAGE_ARTIFACTS) {
				writeFileSync(resolve(left, artifact), `same:${artifact}`);
				writeFileSync(resolve(right, artifact), `same:${artifact}`);
			}
			return { left, right };
		}

		const extra = artifactPair();
		writeFileSync(resolve(extra.left, "snippet.js"), "unexpected");
		expect(() =>
			assertWasmArtifactSetsEqual(
				extra.left,
				extra.right,
				contract.executor.artifactCount,
			),
		).toThrow("path set does not match");

		const directory = artifactPair();
		mkdirSync(resolve(directory.right, "nested"));
		expect(() =>
			assertWasmArtifactSetsEqual(
				directory.left,
				directory.right,
				contract.executor.artifactCount,
			),
		).toThrow("unexpected directory");

		const link = artifactPair();
		symlinkSync("package.json", resolve(link.left, "linked-package"));
		expect(() =>
			assertWasmArtifactSetsEqual(
				link.left,
				link.right,
				contract.executor.artifactCount,
			),
		).toThrow("symbolic link");

		const special = artifactPair();
		execFileSync("mkfifo", [resolve(special.right, "artifact.pipe")]);
		expect(() =>
			assertWasmArtifactSetsEqual(
				special.left,
				special.right,
				contract.executor.artifactCount,
			),
		).toThrow("special path");

		const mismatch = artifactPair();
		renameSync(
			resolve(mismatch.right, "package.json"),
			resolve(mismatch.right, "renamed-package.json"),
		);
		expect(() =>
			assertWasmArtifactSetsEqual(
				mismatch.left,
				mismatch.right,
				contract.executor.artifactCount,
			),
		).toThrow("path set does not match");

		const wrongCount = artifactPair();
		expect(() =>
			assertWasmArtifactSetsEqual(wrongCount.left, wrongCount.right, 5),
		).toThrow("artifact contract count");
	});

	test("shared finalizer removes wasm-pack gitignore and derives the publish allowlist from the exact-six contract", () => {
		const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-finalize-"));
		roots.push(root);
		writeFileSync(resolve(root, ".gitignore"), "*\n");
		for (const artifact of WASM_PACKAGE_ARTIFACTS) {
			const contents =
				artifact === "package.json"
					? `${JSON.stringify({ name: "unfinalized" })}\n`
					: artifact === "waddle_xmpp_client_wasm.d.ts"
						? MINIMAL_RAW_WASM_PACK_DECLARATIONS
						: `fixture:${artifact}`;
			writeFileSync(resolve(root, artifact), contents);
		}

		finalizeWasmPackage(root, "a".repeat(64));
		expect(existsSync(resolve(root, ".gitignore"))).toBe(false);
		expect(readdirSync(root).sort()).toEqual(
			[...WASM_PACKAGE_ARTIFACTS].sort(),
		);
		const pkg = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
		expect(pkg).toMatchObject({
			name: "@waddle/xmpp-client-wasm",
			version: `0.0.0-wasm-${"a".repeat(64)}`,
			publishConfig: {
				registry: GITHUB_PACKAGES_REGISTRY,
				access: "public",
			},
		});
		expect(pkg.files).toEqual(
			WASM_PACKAGE_ARTIFACTS.filter((artifact) => artifact !== "package.json"),
		);
	});

	test("real Bun pack includes every canonical artifact and excludes temporary build files", () => {
		const root = mkdtempSync(resolve(tmpdir(), "waddle-wasm-pack-"));
		roots.push(root);
		const packageDir = resolve(root, "package");
		const archiveDir = resolve(root, "archives");
		mkdirSync(packageDir);
		mkdirSync(archiveDir);
		for (const artifact of WASM_PACKAGE_ARTIFACTS) {
			const contents =
				artifact === "package.json"
					? `${JSON.stringify({ name: "unfinalized" })}\n`
					: artifact === "waddle_xmpp_client_wasm.d.ts"
						? MINIMAL_RAW_WASM_PACK_DECLARATIONS
						: `fixture:${artifact}`;
			writeFileSync(resolve(packageDir, artifact), contents);
		}
		writeFileSync(
			resolve(packageDir, "temporary-build.log"),
			"not publishable\n",
		);

		finalizeWasmPackage(packageDir, "b".repeat(64));
		execFileSync("bun", ["pm", "pack", "--dry-run", "--ignore-scripts"], {
			cwd: packageDir,
			stdio: "ignore",
		});
		execFileSync(
			"bun",
			[
				"pm",
				"pack",
				"--destination",
				archiveDir,
				"--ignore-scripts",
				"--quiet",
			],
			{ cwd: packageDir, stdio: "ignore" },
		);

		const archives = readdirSync(archiveDir, { withFileTypes: true });
		expect(archives).toHaveLength(1);
		expect(archives[0]?.isFile()).toBe(true);
		expect(archives[0]?.name.endsWith(".tgz")).toBe(true);
		const packed = packedArtifactNames(
			resolve(archiveDir, archives[0]?.name ?? "missing-archive.tgz"),
		).sort();

		expect(packed).toEqual([...WASM_PACKAGE_ARTIFACTS].sort());
		expect(packed).not.toContain("temporary-build.log");
	});

	test("derives a deterministic unique valid SemVer prerelease from the full build identity", () => {
		const firstBuild = "0".repeat(64);
		const secondBuild = "f".repeat(64);
		const firstVersion = wasmPackageVersion(firstBuild);
		expect(wasmPackageVersion(firstBuild)).toBe(firstVersion);
		expect(wasmPackageVersion(secondBuild)).not.toBe(firstVersion);
		expect(firstVersion).toMatch(/^0\.0\.0-wasm-[0-9a-f]{64}$/u);
		expect(firstVersion.endsWith(firstBuild)).toBe(true);
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

	test("default publisher uses GitHub Packages, idempotent retry, and a sanitized token environment", () => {
		const workflow = readFileSync(
			resolve(REAL_REPO_ROOT, ".github/workflows/waddle-chat-publishwasm.yml"),
			"utf8",
		);
		expect(workflow).toContain("GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}");
		expect(() =>
			buildAndPublishWasm({
				environment: { NODE_AUTH_TOKEN: "legacy-token" },
			}),
		).toThrow("GITHUB_TOKEN is required");

		const secret = "publisher-secret-must-not-leak";
		const built: string[] = [];
		const logs: string[] = [];
		let invocation:
			| {
					command: string;
					args: string[];
					options: { cwd: string; env: Record<string, string> };
					packageJson: string;
			  }
			| undefined;
		buildAndPublishWasm({
			environment: {
				GITHUB_TOKEN: secret,
				NODE_AUTH_TOKEN: "legacy-node-token",
				NPM_TOKEN: "legacy-npm-token",
				NPM_CONFIG_TOKEN: "legacy-config-token",
				NPM_CONFIG_REGISTRY: "https://registry.example.invalid",
				PATH: "/tools",
			},
			loadIdentity: () => ({ buildId: "d".repeat(64), contract }),
			runBuild: ({ outDir }: { outDir: string }) => {
				built.push(outDir);
				for (const artifact of WASM_PACKAGE_ARTIFACTS) {
					writeFileSync(
						resolve(outDir, artifact),
						artifact === "package.json"
							? JSON.stringify({
									name: "@waddle/xmpp-client-wasm",
									version: wasmPackageVersion("d".repeat(64)),
								})
							: artifact === "waddle_xmpp_client_wasm.d.ts"
								? MINIMAL_RAW_WASM_PACK_DECLARATIONS
							: `same:${artifact}`,
					);
				}
				finalizeWasmPackage(outDir, "d".repeat(64));
			},
			publishExecute: (
				command: string,
				args: string[],
				options: { cwd: string; env: Record<string, string> },
			) => {
				invocation = {
					command,
					args,
					options,
					packageJson: readFileSync(
						resolve(options.cwd, "package.json"),
						"utf8",
					),
				};
			},
			log: (message: string) => logs.push(message),
		});

		expect(invocation?.command).toBe("bun");
		expect(invocation?.args).toEqual([
			"publish",
			"--access",
			"public",
			"--registry",
			GITHUB_PACKAGES_REGISTRY,
			"--tolerate-republish",
		]);
		expect(invocation?.options.cwd).toBe(built[0]);
		expect(JSON.parse(invocation?.packageJson ?? "{}").files).toEqual(
			WASM_PACKAGE_ARTIFACTS.filter((artifact) => artifact !== "package.json"),
		);
		expect(invocation?.options.env).toMatchObject({
			PATH: "/tools",
			NPM_CONFIG_TOKEN: secret,
		});
		for (const removed of [
			"GITHUB_TOKEN",
			"NODE_AUTH_TOKEN",
			"NPM_TOKEN",
			"NPM_CONFIG_REGISTRY",
		]) {
			expect(invocation?.options.env).not.toHaveProperty(removed);
		}
		expect(JSON.stringify(invocation?.args)).not.toContain(secret);
		expect(invocation?.packageJson).not.toContain(secret);
		expect(logs.join("\n")).not.toContain(secret);
	});

	test("publishes only after two isolated artifact sets match", () => {
		const built: string[] = [];
		const published: string[] = [];
		buildAndPublishWasm({
			environment: { GITHUB_TOKEN: "test-token" },
			loadIdentity: () => ({ buildId: "e".repeat(64), contract }),
			runBuild: ({ outDir }: { outDir: string }) => {
				built.push(outDir);
				for (const artifact of WASM_PACKAGE_ARTIFACTS) {
					writeFileSync(resolve(outDir, artifact), `same:${artifact}`);
				}
			},
			publish: (outDir: string) => {
				expect(built).toHaveLength(2);
				expect(outDir).toBe(built[0]);
				for (const artifact of WASM_PACKAGE_ARTIFACTS) {
					expect(readFileSync(resolve(outDir, artifact), "utf8")).toBe(
						`same:${artifact}`,
					);
				}
				published.push(outDir);
			},
			log: () => {},
		});
		expect(built).toHaveLength(2);
		expect(built[0]).not.toBe(built[1]);
		expect(published).toEqual([built[0]]);
	});

	test("never publishes when either isolated artifact set diverges", () => {
		let buildIndex = 0;
		let publishCount = 0;
		expect(() =>
			buildAndPublishWasm({
				environment: { GITHUB_TOKEN: "test-token" },
				loadIdentity: () => ({ buildId: "f".repeat(64), contract }),
				runBuild: ({ outDir }: { outDir: string }) => {
					const currentBuild = buildIndex++;
					for (const artifact of WASM_PACKAGE_ARTIFACTS) {
						const contents =
							currentBuild === 1 &&
							artifact === "waddle_xmpp_client_wasm_bg.wasm"
								? "diverged"
								: `same:${artifact}`;
						writeFileSync(resolve(outDir, artifact), contents);
					}
				},
				publish: () => {
					publishCount += 1;
				},
				log: () => {},
			}),
		).toThrow("waddle_xmpp_client_wasm_bg.wasm");
		expect(buildIndex).toBe(2);
		expect(publishCount).toBe(0);
	});
});
