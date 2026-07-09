import { describe, expect, test } from "bun:test";
import { readdir } from "node:fs/promises";
import { resolve } from "node:path";
import { requirePrivacySafeFeature } from "../scripts/switchable-baseline/gate-evidence/capability-contract";
import {
  CAPABILITY_SERVER_SOURCE_PATHS,
  FARO_WEB_SOURCE_PATHS,
  GATE_ZERO_SOURCE_PATHS,
  TELEMETRY_SERVER_SOURCE_PATHS,
} from "../scripts/switchable-baseline/source-contract";

describe("Gate 0 implementation-source hardening", () => {
	test("freezes one exhaustive implementation source contract", async () => {
		const sourceRoot = resolve(import.meta.dir, "..");
		expect(new Set(GATE_ZERO_SOURCE_PATHS).size).toBe(GATE_ZERO_SOURCE_PATHS.length);
		for (const path of GATE_ZERO_SOURCE_PATHS) {
			expect(await Bun.file(resolve(sourceRoot, path)).exists()).toBeTrue();
		}
		for (const file of await readdir(resolve(sourceRoot, "chat/src/lib/telemetry"))) {
			if (file.endsWith(".ts")) {
				expect(FARO_WEB_SOURCE_PATHS).toContain(`chat/src/lib/telemetry/${file}` as never);
			}
		}
		for (const file of await readdir(resolve(
			sourceRoot,
			"server/crates/waddle-xmpp-client/src/capability_evidence",
		))) {
			if (file.endsWith(".rs") && file !== "tests.rs") {
				expect(CAPABILITY_SERVER_SOURCE_PATHS).toContain(
					`server/crates/waddle-xmpp-client/src/capability_evidence/${file}` as never,
				);
			}
		}
		for (const path of [
			"server/crates/waddle-server/src/server/disco_targets/contract.rs",
			"server/crates/waddle-server/src/server/disco_targets/features.rs",
			"server/crates/waddle-server/src/server/disco_targets/identities.rs",
			"server/crates/waddle-server/src/server/routes/websocket/handlers/iq/extension_forms.rs",
			"server/crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_discovery.rs",
			"server/crates/waddle-server/src/server/routes/websocket/state.rs",
			"server/crates/waddle-xmpp-core/src/pubsub/pep.rs",
			"server/crates/waddle-xmpp/src/admin.rs",
			"server/crates/waddle-xmpp/src/disco/info.rs",
			"server/crates/waddle-xmpp/src/isr.rs",
			"server/crates/waddle-xmpp/src/parser/ns.rs",
			"server/crates/waddle-xmpp/src/pubsub/pep.rs",
			"server/crates/waddle-xmpp/src/xep/xep0430.rs",
			"server/crates/waddle-xmpp/src/xep/xep0433.rs",
		]) expect(CAPABILITY_SERVER_SOURCE_PATHS).toContain(path as never);
		for (const path of [
			"server/crates/waddle-xmpp/src/prometheus/process_start.rs",
			"server/crates/waddle-xmpp/src/auth/oauthbearer.rs",
			"server/crates/waddle-xmpp/src/auth/scram/parser.rs",
			"server/crates/waddle-xmpp/src/protocol/frame.rs",
			"server/crates/waddle-xmpp/src/protocol/phase.rs",
			"server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs",
			"server/crates/waddle-server/src/server/routes/websocket/parse_errors.rs",
			"server/crates/waddle-server/src/server/routes/websocket/transport_xml.rs",
			"server/charts/waddle-server/templates/_helpers.tpl",
			"server/charts/waddle-server/templates/deployment.yaml",
			"server/charts/waddle-server/templates/service.yaml",
			"infrastructure/waddle.cloud/gitops/grafana-alloy/helmrelease.yaml",
		]) expect(TELEMETRY_SERVER_SOURCE_PATHS).toContain(path as never);
		for (const path of [
			"chat/scripts/build-identity.mjs",
			"chat/scripts/resolve-commit-sha.mjs",
			"chat/src/auth/session.ts",
			"chat/src/lib/xmpp/client-events.ts",
			"chat/src/lib/xmpp/send-types.ts",
			"chat/src/lib/xmpp/xmpp-instrumentation.ts",
		]) expect(FARO_WEB_SOURCE_PATHS).toContain(path as never);
		const taskSource = await Bun.file(resolve(sourceRoot, "server/env.cue")).text();
		expect(taskSource.match(/_gateZeroEvidenceSourceInputs/g)?.length).toBe(3);
		const taskInputBlock = taskSource.slice(
			taskSource.indexOf("let _gateZeroEvidenceSourceInputs"),
			taskSource.indexOf("let _capabilityCollectionEnv"),
		);
		const taskInputPatterns = [...taskInputBlock.matchAll(/"([^"]+)"/g)]
			.map((match) => match[1]);
		for (const repositoryPath of GATE_ZERO_SOURCE_PATHS) {
			const taskPath = repositoryPath.startsWith("server/")
				? repositoryPath.slice("server/".length)
				: `../${repositoryPath}`;
			expect(
				taskInputPatterns.some((pattern) => new Bun.Glob(pattern).match(taskPath)),
				`${repositoryPath} must invalidate every Gate 0 evidence task`,
			).toBeTrue();
		}
		expect(taskSource).toContain('"../chat/src/lib/telemetry/**"');
		expect(taskSource).toContain('"crates/waddle-xmpp/src/prometheus/**"');
		expect(taskSource).toContain('"crates/waddle-xmpp/src/auth/**"');
		expect(taskSource).toContain('"crates/waddle-xmpp/src/disco/**"');
		expect(taskSource).toContain('"crates/waddle-xmpp/src/protocol/frame.rs"');
		expect(taskSource).toContain('"crates/waddle-xmpp-core/src/pubsub/pep.rs"');
		expect(taskSource).toContain('"crates/waddle-xmpp-client/src/client.rs"');
		expect(taskSource).toContain('"crates/waddle-xmpp-client/src/config.rs"');
		expect(taskSource).toContain('"crates/waddle-xmpp-client/src/discovery.rs"');
		expect(taskSource).toContain('"crates/waddle-xmpp-client/src/error.rs"');
		expect(taskSource).toContain('"crates/waddle-xmpp-client/src/event.rs"');
		expect(taskSource).toContain('"crates/waddle-xmpp-client/src/runtime/**"');
		expect(taskSource).toContain('"crates/waddle-xmpp-client/src/transport/**"');
		expect(taskSource).toContain('"crates/waddle-xmpp-core/src/disco_target.rs"');
		expect(taskSource).toContain('"charts/waddle-server/templates/_helpers.tpl"');
		expect(taskSource).toContain('"charts/waddle-server/templates/service.yaml"');
		expect(taskSource).toContain('"../chat/src/build-identity-contract.ts"');
	});

	test("accepts only static versioned checked-in Waddle namespaces", () => {
		expect(() => requirePrivacySafeFeature("urn:waddle:extension:installed:0", "feature"))
			.not.toThrow();
		for (const value of [
			"urn:waddle:extension:YWxpY2VAZXhhbXBsZS5jb20",
			"urn:waddle:account:550e8400-e29b-41d4-a716-446655440000",
			"urn:waddle:token:eyJhbGciOiJIUzI1NiJ9",
			"urn:waddle:extension:alice%40example.com:0",
			"urn:waddle:extension:alice@example.com:0",
		]) expect(() => requirePrivacySafeFeature(value, "feature")).toThrow();
	});
});
