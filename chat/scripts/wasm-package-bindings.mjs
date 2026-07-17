import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { WASM_PACKAGE_ARTIFACTS } from "./wasm-build-executor.mjs";

const CANONICAL_BUILD_ID = /^[0-9a-f]{64}$/u;

export function wasmPackageVersion(buildId) {
	if (!CANONICAL_BUILD_ID.test(buildId)) {
		throw new Error("WASM build ID must be a full lowercase SHA-256 digest");
	}
	return `0.0.0-wasm-${buildId}`;
}

export function renderWasmWrapper(buildId) {
	wasmPackageVersion(buildId);
	return `/* @ts-self-types="./waddle_xmpp_client_wasm.d.ts" */
import wasmUrl from "./waddle_xmpp_client_wasm_bg.wasm?url&b=${buildId}";
import * as bgModule from "./waddle_xmpp_client_wasm_bg.js?b=${buildId}";
import { __wbg_set_wasm } from "./waddle_xmpp_client_wasm_bg.js?b=${buildId}";

let initPromise;

export default async function init() {
  if (!initPromise) {
    initPromise = (async () => {
      // In dev mode bypass the browser HTTP/WebAssembly cache so that a fresh
      // REBUILD_WASM=1 build is picked up without a manual hard-refresh.
      // In production the URL is content-hashed, so "default" is fine.
      const cache = import.meta.env.DEV ? "no-store" : "default";
      const response = await fetch(wasmUrl, { cache });
      const bytes = await response.arrayBuffer();
      const { instance } = await WebAssembly.instantiate(bytes, {
        // The import-object key must match the literal string the WASM binary
        // imports from — wasm-pack writes "./waddle_xmpp_client_wasm_bg.js"
        // into the binary, with no query string.
        "./waddle_xmpp_client_wasm_bg.js": bgModule,
      });
      __wbg_set_wasm(instance.exports);
    })();
  }
  return initPromise;
}

// Re-export every public binding wasm-pack emitted — classes (WaddleClient,
// WaddleConfig, …) AND Rust free functions (xep0392_consistent_hue,
// xep0392_consistent_color, …). A hand-curated list silently drops new
// #[wasm_bindgen] free functions until somebody notices the chat crashing.
export * from "./waddle_xmpp_client_wasm_bg.js?b=${buildId}";
`;
}

const TYPED_DECLARATIONS_MARKER =
	"export type WaddleSendMessageOutcome =";

const TYPED_DECLARATIONS = `export type WaddleSendMessageOutcome =
    | { readonly kind: "sent"; readonly stanza_id: string }
    | { readonly kind: "not-connected" }
    | { readonly kind: "invalid-recipient" }
    | { readonly kind: "invalid-options" }
    | { readonly kind: "stanza-error" }
    | { readonly kind: "transport-error" }
    | { readonly kind: "error" };

export interface WaddleSendOptions {
    readonly stanza_id?: string;
    readonly subject?: string;
    readonly reply?: {
        readonly author_jid: string;
        readonly message_id: string;
    };
    readonly fallback?: { readonly start: number; readonly end: number };
    readonly thread?: { readonly id: string; readonly parent?: string };
    readonly link_preview_token?: string;
    readonly request_displayed_marker?: boolean;
    readonly muc_pm?: boolean;
    readonly shared_files?: readonly {
        readonly url: string;
        readonly name?: string;
        readonly media_type?: string;
        readonly size?: number;
        readonly width?: number;
        readonly height?: number;
        readonly disposition: string;
        readonly encrypted?: {
            readonly cipher: string;
            readonly key_b64: string;
            readonly iv_b64: string;
            readonly hashes: readonly {
                readonly algo: string;
                readonly value_b64: string;
            }[];
            readonly sources: readonly string[];
        };
    }[];
    readonly markup_spans?: readonly {
        readonly span_type: string;
        readonly start: number;
        readonly end: number;
        readonly uri?: string;
    }[];
    readonly references?: readonly {
        readonly ref_type: string;
        readonly uri: string;
        readonly begin: number;
        readonly end: number;
        readonly anchor?: string;
    }[];
}

export type WaddleDriverErrorReason =
    | "core-error"
    | "invalid-transport-scheme"
    | "missing-websocket-host"
    | "empty-resource"
    | "empty-stanza-id"
    | "request-id-exhausted"
    | "duplicate-request"
    | "duplicate-stanza-correlation"
    | "unknown-request"
    | "unknown-stanza-correlation"
    | "invalid-phase-transition"
    | "invalid-state-transition"
    | "missing-stream-feature"
    | "invalid-stream-features"
    | "invalid-sasl-failure"
    | "invalid-bind-response"
    | "authentication-rejected"
    | "websocket-connect-timeout"
    | "websocket-write-timeout"
    | "iq-timeout"
    | "websocket-transport-error"
    | "empty-transport-frame"
    | "transport-frame-too-large"
    | "invalid-transport-frame"
    | "invalid-stream-open-to"
    | "invalid-stream-open-from"
    | "unsupported-stream-version"
    | "unsupported-websocket-message"
    | "transport-closed"
    | "request-cancelled"
    | "disconnected"
    | "invalid-resume-stanza"
    | "push-registration-error"
    | "stanza-error";

export type WaddleAuthenticationCondition =
    | "aborted"
    | "account-disabled"
    | "credentials-expired"
    | "encryption-required"
    | "incorrect-encoding"
    | "invalid-authzid"
    | "invalid-mechanism"
    | "malformed-request"
    | "mechanism-too-weak"
    | "not-authorized"
    | "temporary-auth-failure"
    | "unknown";

export type WaddleStreamErrorCondition =
    | "bad-format"
    | "bad-namespace-prefix"
    | "conflict"
    | "connection-timeout"
    | "host-gone"
    | "host-unknown"
    | "improper-addressing"
    | "internal-server-error"
    | "invalid-from"
    | "invalid-namespace"
    | "invalid-xml"
    | "not-authorized"
    | "not-well-formed"
    | "policy-violation"
    | "remote-connection-failed"
    | "reset"
    | "resource-constraint"
    | "restricted-xml"
    | "see-other-host"
    | "system-shutdown"
    | "undefined-condition"
    | "unsupported-encoding"
    | "unsupported-feature"
    | "unsupported-stanza-type"
    | "unsupported-version";

export type WaddleControlErrorPayload =
    | {
        readonly kind: "driver-error";
        readonly reason: WaddleDriverErrorReason;
        readonly authenticationCondition?: WaddleAuthenticationCondition | null;
    }
    | {
        readonly kind: "stream-error";
        readonly condition: WaddleStreamErrorCondition;
        readonly streamManagementError?: {
            readonly kind: "handled-count-too-high";
            readonly h: number;
            readonly sendCount: number;
        } | null;
    };

export type WaddleStreamManagementTelemetry =
    | { readonly kind: "ack-request"; readonly attempt: number; readonly unacked: number }
    | {
        readonly kind: "ack-observed";
        readonly progressed: boolean;
        readonly latencyMs?: number | null;
        readonly unacked: number;
    }
    | { readonly kind: "ack-request-timeout"; readonly unacked: number }
    | {
        readonly kind: "ack-progress-stalled";
        readonly unacked: number;
        readonly elapsedMs: number;
    };

export type WaddleSessionLifecycle = "fresh" | "resumed";
export type WaddleCallEventPayload = Readonly<Record<string, unknown>>;
export type WaddleMessagePayload = Readonly<Record<string, unknown>>;
export type WaddlePresencePayload = Readonly<Record<string, unknown>>;
export type WaddlePubsubEventPayload = Readonly<Record<string, unknown>>;
export interface WaddleMdsDisplayedEntry {
    readonly chat_id: string;
    readonly stanza_id: string;
    readonly stanza_id_by: string;
}

`;

const TYPED_BINDING_SIGNATURES = [
	[
		"    get_resume_state(): any;",
		"    get_resume_state(): WaddleResumeStateSnapshot | null;",
	],
	[
		"    with_resume_state(state: any): void;",
		"    with_resume_state(state: WaddleResumeStateSnapshot): void;",
	],
	[
		"    send_chat_message(peer_jid: string, body: string, options: any): Promise<any>;",
		"    send_chat_message(peer_jid: string, body: string, options: WaddleSendOptions): Promise<WaddleSendMessageOutcome>;",
	],
	[
		"    send_groupchat_message(room_jid: string, body: string, options: any): Promise<any>;",
		"    send_groupchat_message(room_jid: string, body: string, options: WaddleSendOptions): Promise<WaddleSendMessageOutcome>;",
	],
	[
		"    set_on_call(cb: Function): void;",
		"    set_on_call(cb: (event: WaddleCallEventPayload) => void): void;",
	],
	[
		"    set_on_connected(cb: Function): void;",
		"    set_on_connected(cb: () => void): void;",
	],
	[
		"    set_on_disconnected(cb: Function): void;",
		"    set_on_disconnected(cb: () => void): void;",
	],
	[
		"    set_on_error(cb: Function): void;",
		"    set_on_error(cb: (error: WaddleControlErrorPayload) => void): void;",
	],
	[
		"    set_on_mds_displayed(cb: Function): void;",
		"    set_on_mds_displayed(cb: (entry: WaddleMdsDisplayedEntry) => void): void;",
	],
	[
		"    set_on_message(cb: Function): void;",
		"    set_on_message(cb: (message: WaddleMessagePayload) => void): void;",
	],
	[
		"    set_on_message_delivery_acked(cb: Function): void;",
		"    set_on_message_delivery_acked(cb: (stanzaId: string) => void): void;",
	],
	[
		"    set_on_message_delivery_failed(cb: Function): void;",
		"    set_on_message_delivery_failed(cb: (stanzaId: string) => void): void;",
	],
	[
		"    set_on_presence(cb: Function): void;",
		"    set_on_presence(cb: (presence: WaddlePresencePayload) => void): void;",
	],
	[
		"    set_on_pubsub_event(cb: Function): void;",
		"    set_on_pubsub_event(cb: (event: WaddlePubsubEventPayload) => void): void;",
	],
	[
		"    set_on_session_lifecycle(cb: Function): void;",
		"    set_on_session_lifecycle(cb: (event: WaddleSessionLifecycle) => void): void;",
	],
	[
		"    set_on_stream_management(cb: Function): void;",
		"    set_on_stream_management(cb: (event: WaddleStreamManagementTelemetry) => void): void;",
	],
];

const FORBIDDEN_LEGACY_RESUME_DECLARATIONS = [
	"get_resume_state_handle(",
	"with_resume_state_entries(",
	"with_resume_state_entries_with_max(",
	"with_resume_state_handle(",
	"with_resume_state_with_max(",
	"export class WaddleResumeState",
	"hasUnackedOutbound",
];

const REQUIRED_RESUME_DECLARATIONS = [
	'export type WaddleResumeStanzaKind = "message" | "presence" | "iq";',
	"export interface WaddleResumeXmlName {",
	"export interface WaddleResumeXmlAttribute {",
	"export type WaddleResumeXmlToken =",
	"export interface WaddleResumeStanzaSnapshot {",
	"    readonly stanzaKind: WaddleResumeStanzaKind;",
	"    readonly tokens: WaddleResumeXmlToken[];",
	"export interface WaddleResumeEntrySnapshot {",
	"    readonly stanza: WaddleResumeStanzaSnapshot;",
	"    readonly sentAtEpochMs: number;",
	"export interface WaddleResumeStateSnapshot {",
	"    readonly previd: string;",
	"    readonly inboundH: number;",
	"    readonly outboundH: number;",
	"    readonly unhandledOutboundEntries: WaddleResumeEntrySnapshot[];",
	"    readonly maxResumeSeconds?: number;",
];

function replaceGeneratedSignature(source, raw, typed) {
	const rawOccurrences = source.split(raw).length - 1;
	const typedOccurrences = source.split(typed).length - 1;
	if (rawOccurrences === 1 && typedOccurrences === 0) {
		return source.replace(raw, typed);
	}
	if (rawOccurrences === 0 && typedOccurrences === 1) {
		return source;
	}
	throw new Error(
		`WASM declaration drift: expected exactly one generated or typed signature for ${typed.trim()}`,
	);
}

export function renderTypedWasmDeclarations(source) {
	for (const declaration of REQUIRED_RESUME_DECLARATIONS) {
		if (!source.includes(declaration)) {
			throw new Error(
				`WASM declaration drift: missing typed resume declaration ${declaration}`,
			);
		}
	}
	for (const declaration of FORBIDDEN_LEGACY_RESUME_DECLARATIONS) {
		if (source.includes(declaration)) {
			throw new Error(
				`WASM declaration drift: legacy resume surface ${declaration}`,
			);
		}
	}
	const classMarker = "export class WaddleClient";
	const classOffset = source.indexOf(classMarker);
	if (classOffset < 0) {
		throw new Error("WASM declaration drift: missing WaddleClient class");
	}

	let rendered = source;
	if (!rendered.includes(TYPED_DECLARATIONS_MARKER)) {
		rendered =
			rendered.slice(0, classOffset) +
			TYPED_DECLARATIONS +
			rendered.slice(classOffset);
	} else if (!rendered.includes(TYPED_DECLARATIONS)) {
		throw new Error(
			"WASM declaration drift: generated typed declaration block changed",
		);
	}
	for (const [raw, typed] of TYPED_BINDING_SIGNATURES) {
		rendered = replaceGeneratedSignature(rendered, raw, typed);
	}
	return rendered;
}

export function finalizeWasmPackage(outDir, buildId) {
	// wasm-pack emits this repository convenience file beside the package.
	// It is not a publish artifact and would violate the exact-six attestation
	// contract, so the shared local/publish finalizer removes it explicitly.
	rmSync(resolve(outDir, ".gitignore"), { force: true });

	const pkgJsonPath = resolve(outDir, "package.json");
	const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf8"));
	pkg.name = "@waddle/xmpp-client-wasm";
	pkg.version = wasmPackageVersion(buildId);
	pkg.files = WASM_PACKAGE_ARTIFACTS.filter(
		(artifact) => artifact !== "package.json",
	);
	pkg.publishConfig = {
		registry: "https://npm.pkg.github.com",
		access: "public",
	};
	writeFileSync(pkgJsonPath, `${JSON.stringify(pkg, null, 2)}\n`);

	writeFileSync(
		resolve(outDir, "waddle_xmpp_client_wasm.js"),
		renderWasmWrapper(buildId),
	);

	const dtsPath = resolve(outDir, "waddle_xmpp_client_wasm.d.ts");
	let dts = renderTypedWasmDeclarations(readFileSync(dtsPath, "utf8"));
	if (!dts.includes("export default function init()")) {
		dts = `${dts}\nexport default function init(): Promise<void>;\n`;
	}
	writeFileSync(dtsPath, dts);
}
