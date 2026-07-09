#!/usr/bin/env bun

import { resolve } from "node:path";
import {
	verifyGateZeroEvidencePackage,
} from "./switchable-baseline/finalize";
import { finalizeGateZeroEvidence } from "./switchable-baseline/finalize/all";
import type { EvidenceDeploymentScope } from "./switchable-baseline/gate-evidence";
import { REPOSITORY_ROOT } from "./switchable-baseline/model";
import type { RepositorySourceAtCommitReader } from "./switchable-baseline/gate-evidence/filesystem";
import type { LiveCollectionAttestationVerifier } from "./switchable-baseline/attestation";
import type { ReleaseArtifactProvenanceVerifier } from "./switchable-baseline/release-artifact-provenance";

type Mode = "all" | "verify";

const commonOptions = [
	"server-commit",
	"web-commit",
	"start",
	"end",
	"captured-at",
	"job",
	"deployment-environment",
	"cluster",
	"namespace",
	"expected-replicas",
	"identity-metric",
	"target-signal-id",
	"identity-lookback-seconds",
] as const;

function parseOptions(values: string[], allowed: readonly string[]): Map<string, string> {
	const accepted = new Set(allowed);
	const result = new Map<string, string>();
	for (let index = 0; index < values.length; index += 2) {
		const option = values[index];
		const value = values[index + 1];
		if (!option?.startsWith("--") || value === undefined || value.startsWith("--")) {
			throw new Error("finalizer arguments must be --name value pairs");
		}
		const name = option.slice(2);
		if (!accepted.has(name)) throw new Error(`unknown finalizer option --${name}`);
		if (result.has(name)) throw new Error(`duplicate finalizer option --${name}`);
		result.set(name, value);
	}
	for (const name of allowed) {
		if (!result.get(name)) throw new Error(`missing required finalizer option --${name}`);
	}
	return result;
}

function value(options: Map<string, string>, name: string): string {
	const entry = options.get(name);
	if (!entry) throw new Error(`missing required finalizer option --${name}`);
	return entry;
}

function integer(options: Map<string, string>, name: string): number {
	const raw = value(options, name);
	if (!/^\d+$/.test(raw)) throw new Error(`--${name} must be a positive integer`);
	return Number(raw);
}

function metadata(options: Map<string, string>) {
	const deploymentScope: EvidenceDeploymentScope = {
		job: value(options, "job"),
		environment: value(options, "deployment-environment"),
		cluster: value(options, "cluster"),
		namespace: value(options, "namespace"),
		expectedReplicas: integer(options, "expected-replicas"),
		identityMetric: value(options, "identity-metric"),
		targetSignalId: value(options, "target-signal-id"),
		identityLookbackSeconds: integer(options, "identity-lookback-seconds"),
	};
	return {
		release: {
			serverCommit: value(options, "server-commit"),
			webCommit: value(options, "web-commit"),
		},
		window: { start: value(options, "start"), end: value(options, "end") },
		capturedAt: value(options, "captured-at"),
		deploymentScope,
	};
}

export async function runSwitchableBaselineFinalizer(
	argumentsList: string[],
	repositoryRoot = REPOSITORY_ROOT,
	sourceAtCommit?: RepositorySourceAtCommitReader,
	attestationVerifier?: LiveCollectionAttestationVerifier,
	releaseArtifactVerifier?: ReleaseArtifactProvenanceVerifier,
): Promise<unknown> {
	const mode = argumentsList[0] as Mode | undefined;
	if (!mode || !["all", "verify"].includes(mode)) {
		throw new Error("finalizer mode must be all or verify");
	}
	if (mode === "verify") {
		if (argumentsList.length !== 1) throw new Error("verify mode accepts no options");
		return verifyGateZeroEvidencePackage(
			repositoryRoot,
			sourceAtCommit,
			attestationVerifier,
			releaseArtifactVerifier,
		);
	}
	const faroOptions = [
		"faro-browser-auth-bootstrap",
		"faro-browser-message-ack-latency",
		"faro-browser-session-lifecycle",
		"faro-browser-reconnect-duration",
	] as const;
	const options = parseOptions(
		argumentsList.slice(1),
		[
			...commonOptions,
			"live-disco",
			"prometheus",
			...faroOptions,
			"collection-subject",
			"attestation-bundle",
		],
	);
	await finalizeGateZeroEvidence({
		repositoryRoot,
		sourceAtCommit,
		...metadata(options),
		liveDiscoPath: resolve(value(options, "live-disco")),
		prometheusPath: resolve(value(options, "prometheus")),
		faroPaths: {
			"faro-browser-auth-bootstrap": resolve(value(options, "faro-browser-auth-bootstrap")),
			"faro-browser-message-ack-latency": resolve(
				value(options, "faro-browser-message-ack-latency"),
			),
			"faro-browser-session-lifecycle": resolve(
				value(options, "faro-browser-session-lifecycle"),
			),
			"faro-browser-reconnect-duration": resolve(
				value(options, "faro-browser-reconnect-duration"),
			),
		},
		subjectPath: resolve(value(options, "collection-subject")),
		bundlePath: resolve(value(options, "attestation-bundle")),
		attestationVerifier,
		releaseArtifactVerifier,
	});
	return verifyGateZeroEvidencePackage(
		repositoryRoot,
		sourceAtCommit,
		attestationVerifier,
		releaseArtifactVerifier,
	);
}

if (import.meta.main) {
	runSwitchableBaselineFinalizer(process.argv.slice(2))
		.then((result) => console.log(JSON.stringify(result, null, 2)))
		.catch((error: unknown) => {
			console.error(
				`finalize-switchable-baseline: ${error instanceof Error ? error.message : "unknown failure"}`,
			);
			process.exitCode = 1;
		});
}
