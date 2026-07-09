import { resolve } from "node:path";
import {
	LIVE_COLLECTION_ROLES,
	TRUSTED_ISSUER,
	TRUSTED_REPOSITORY,
	TRUSTED_SOURCE_REF,
	TRUSTED_WORKFLOW,
	parseLiveCollectionSubject,
	sha256Locator,
	type LiveCollectionRole,
	type LiveCollectionSubject,
} from "./attestation";
import { parseDeploymentScope, parseRelease, parseWindow } from "./gate-evidence/common";
import { readPinnedFile } from "./gate-evidence/filesystem";
import { parseJsonDocument } from "./json";
import { commitFilesNoClobber } from "./no-clobber";
import { resolveRestrictedEvidenceInput } from "./filesystem";
import { parseReplicaProvenance } from "./replica-provenance";

const stagedArtifacts: Record<LiveCollectionRole, string> = {
	"live-disco-export": "capability/live-disco-export.json",
	"prometheus-baseline": "prometheus/telemetry-baseline.json",
	"faro-browser-auth-bootstrap": "faro/browser-auth-bootstrap.json",
	"faro-browser-message-ack-latency": "faro/browser-message-ack-latency.json",
	"faro-browser-session-lifecycle": "faro/browser-session-lifecycle.json",
	"faro-browser-reconnect-duration": "faro/browser-reconnect-duration.json",
};

function requiredEnvironment(
	environment: Record<string, string | undefined>,
	name: string,
): string {
	const value = environment[name];
	if (!value) throw new Error(`live collection subject requires ${name}`);
	return value;
}

function canonicalLocator(value: string, protocol: "wss:" | "https:"): string {
	let url: URL;
	try {
		url = new URL(value);
	} catch {
		throw new Error(`live collection source locator must be a valid ${protocol} URL`);
	}
	if (
		url.protocol !== protocol
		|| url.username
		|| url.password
		|| url.search
		|| url.hash
	) throw new Error(`live collection source locator must be credential-free ${protocol}`);
	return url.href;
}

function canonicalArtifactDigest(path: string, role: string): string {
	const snapshot = readPinnedFile(path, `staged ${role}`);
	const value = parseJsonDocument(snapshot.bytes.toString("utf8"), `staged ${role}`);
	const canonical = `${JSON.stringify(value, null, 2)}\n`;
	if (!snapshot.bytes.equals(Buffer.from(canonical))) {
		throw new Error(`staged ${role} must use deterministic canonical JSON`);
	}
	return snapshot.sha256;
}

export async function buildLiveCollectionSubject(input: {
	repositoryRoot: string;
	release: unknown;
	window: unknown;
	deploymentScope: unknown;
	replicaProvenance: unknown;
	releaseArtifactProvenance: unknown;
	environment: Record<string, string | undefined>;
}): Promise<{ subject: LiveCollectionSubject; path: string }> {
	const workflowCommit = requiredEnvironment(input.environment, "GITHUB_SHA");
	if (!/^[0-9a-f]{40}$/.test(workflowCommit)) {
		throw new Error("GITHUB_SHA must be a full lowercase Git SHA");
	}
	if (requiredEnvironment(input.environment, "GITHUB_REF") !== TRUSTED_SOURCE_REF) {
		throw new Error("live collection attestation must run from refs/heads/main");
	}
	const workflowRef = requiredEnvironment(input.environment, "GITHUB_WORKFLOW_REF");
	if (workflowRef !== `${TRUSTED_REPOSITORY}/.github/workflows/gate0-live-evidence.yml@${TRUSTED_SOURCE_REF}`) {
		throw new Error("live collection attestation must run from the trusted default-branch workflow");
	}
	const stagingRoot = resolve(input.repositoryRoot, "target/switchable-baseline-inputs");
	const artifacts = [] as Array<{ role: LiveCollectionRole; sha256: string }>;
	for (const role of LIVE_COLLECTION_ROLES) {
		const path = await resolveRestrictedEvidenceInput(
			resolve(stagingRoot, stagedArtifacts[role]),
			input.repositoryRoot,
			role,
		);
		artifacts.push({ role, sha256: canonicalArtifactDigest(path, role) });
	}
	const deploymentScope = parseDeploymentScope(
		input.deploymentScope,
		"live collection deployment scope",
	);
	const release = parseRelease(input.release, "live collection release");
	const replicaProvenance = parseReplicaProvenance(input.replicaProvenance, deploymentScope);
	const faroIngestLocator = canonicalLocator(
		requiredEnvironment(input.environment, "PUBLIC_FARO_URL"),
		"https:",
	);
	const faroQueryLocator = canonicalLocator(
		requiredEnvironment(input.environment, "GRAFANA_FARO_QUERY_URL"),
		"https:",
	);
	if (faroIngestLocator === faroQueryLocator) {
		throw new Error("Faro ingest and query sources must be distinct endpoints");
	}
	const dataSourceUid = requiredEnvironment(input.environment, "GRAFANA_FARO_DATA_SOURCE_UID");
	if (!/^[A-Za-z0-9_-]{1,128}$/.test(dataSourceUid)) {
		throw new Error("GRAFANA_FARO_DATA_SOURCE_UID must be a bounded data-source identifier");
	}
	const subject = parseLiveCollectionSubject({
		schemaVersion: 1,
		evidenceKind: "gate-0-live-collection-subject",
		release,
		window: parseWindow(input.window, "live collection window"),
		deploymentScope,
		replicaProvenance,
		releaseArtifactProvenance: input.releaseArtifactProvenance,
		attestor: {
			repository: TRUSTED_REPOSITORY,
			workflow: TRUSTED_WORKFLOW,
			issuer: TRUSTED_ISSUER,
			sourceRef: TRUSTED_SOURCE_REF,
			workflowCommit,
		},
		sources: {
			xmppLocatorSha256: sha256Locator(canonicalLocator(
				requiredEnvironment(input.environment, "WADDLE_CAPABILITY_ENDPOINT"),
				"wss:",
			)),
			prometheusLocatorSha256: sha256Locator(canonicalLocator(
				requiredEnvironment(input.environment, "GRAFANA_PROMETHEUS_URL"),
				"https:",
			)),
			faroIngestLocatorSha256: sha256Locator(faroIngestLocator),
			faroQuerySource: {
				kind: "grafana-faro-query-api",
				locatorSha256: sha256Locator(faroQueryLocator),
				dataSourceUidSha256: sha256Locator(dataSourceUid),
			},
		},
		artifacts,
	});
	const path = resolve(stagingRoot, "attestation/live-collection-subject.json");
	await commitFilesNoClobber(
		input.repositoryRoot,
		[{ path, contents: `${JSON.stringify(subject, null, 2)}\n` }],
		async () => undefined,
	);
	return { subject, path };
}
