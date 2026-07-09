import { createHash } from "node:crypto";
import { chmod, mkdtemp, open, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import {
	parseDeploymentScope,
	parseRelease,
	parseWindow,
	requireExactKeys,
	requireRecord,
	requireSha256,
	requireString,
	type EvidenceDeploymentScope,
	type EvidenceRelease,
	type EvidenceWindow,
} from "./gate-evidence/common";
import { parseJsonDocument } from "./json";
import {
	parseReplicaProvenance,
	type ReplicaProvenance,
} from "./replica-provenance";
import {
	parseReleaseArtifactProvenance,
	type ReleaseArtifactProvenance,
} from "./release-artifact-provenance";

export const LIVE_COLLECTION_SUBJECT_PATH =
	"docs/evidence/gate-0/attestations/live-collection-subject.json";
export const LIVE_COLLECTION_BUNDLE_PATH =
	"docs/evidence/gate-0/attestations/live-collection.sigstore.json";
export const TRUSTED_REPOSITORY = "waddle-social/waddle";
export const TRUSTED_WORKFLOW =
	"waddle-social/waddle/.github/workflows/gate0-live-evidence.yml";
export const TRUSTED_WORKFLOW_IDENTITY =
	"https://github.com/waddle-social/waddle/.github/workflows/gate0-live-evidence.yml@refs/heads/main";
export const TRUSTED_ISSUER = "https://token.actions.githubusercontent.com";
export const TRUSTED_SOURCE_REF = "refs/heads/main";
export const SLSA_PROVENANCE_V1 = "https://slsa.dev/provenance/v1";

export const LIVE_COLLECTION_ROLES = [
	"live-disco-export",
	"prometheus-baseline",
	"faro-browser-auth-bootstrap",
	"faro-browser-message-ack-latency",
	"faro-browser-session-lifecycle",
	"faro-browser-reconnect-duration",
] as const;

export type LiveCollectionRole = (typeof LIVE_COLLECTION_ROLES)[number];

export interface LiveCollectionSubject {
	schemaVersion: 1;
	evidenceKind: "gate-0-live-collection-subject";
	release: EvidenceRelease;
	window: EvidenceWindow;
	deploymentScope: EvidenceDeploymentScope;
	replicaProvenance: ReplicaProvenance;
	releaseArtifactProvenance: ReleaseArtifactProvenance;
	attestor: {
		repository: typeof TRUSTED_REPOSITORY;
		workflow: typeof TRUSTED_WORKFLOW;
		issuer: typeof TRUSTED_ISSUER;
		sourceRef: typeof TRUSTED_SOURCE_REF;
		workflowCommit: string;
	};
	sources: {
		xmppLocatorSha256: string;
		prometheusLocatorSha256: string;
		faroIngestLocatorSha256: string;
		faroQuerySource: {
			kind: "grafana-faro-query-api";
			locatorSha256: string;
			dataSourceUidSha256: string;
		};
	};
	artifacts: Array<{ role: LiveCollectionRole; sha256: string }>;
}

export type LiveCollectionAttestationVerifier = (input: {
	subjectPath: string;
	bundlePath: string;
	subject: LiveCollectionSubject;
	subjectSha256: string;
	subjectBytes: Uint8Array;
	bundleSha256: string;
	bundleBytes: Uint8Array;
}) => Promise<void>;

function exactLiteral(value: unknown, expected: string, label: string): void {
	if (value !== expected) throw new Error(`${label} must be ${expected}`);
}

export function sha256Locator(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

export function parseLiveCollectionSubject(value: unknown): LiveCollectionSubject {
	const label = "live collection subject";
	const subject = requireRecord(value, label);
	requireExactKeys(subject, [
		"schemaVersion",
		"evidenceKind",
		"release",
		"window",
		"deploymentScope",
		"replicaProvenance",
		"releaseArtifactProvenance",
		"attestor",
		"sources",
		"artifacts",
	], label);
	if (subject.schemaVersion !== 1) throw new Error(`${label}.schemaVersion must be 1`);
	exactLiteral(subject.evidenceKind, "gate-0-live-collection-subject", `${label}.evidenceKind`);
	const attestor = requireRecord(subject.attestor, `${label}.attestor`);
	requireExactKeys(attestor, [
		"repository",
		"workflow",
		"issuer",
		"sourceRef",
		"workflowCommit",
	], `${label}.attestor`);
	exactLiteral(attestor.repository, TRUSTED_REPOSITORY, `${label}.attestor.repository`);
	exactLiteral(attestor.workflow, TRUSTED_WORKFLOW, `${label}.attestor.workflow`);
	exactLiteral(attestor.issuer, TRUSTED_ISSUER, `${label}.attestor.issuer`);
	exactLiteral(attestor.sourceRef, TRUSTED_SOURCE_REF, `${label}.attestor.sourceRef`);
	const workflowCommit = requireString(attestor, "workflowCommit", `${label}.attestor`);
	if (!/^[0-9a-f]{40}$/.test(workflowCommit)) {
		throw new Error(`${label}.attestor.workflowCommit must be a full lowercase Git SHA`);
	}
	const deploymentScope = parseDeploymentScope(
		subject.deploymentScope,
		`${label}.deploymentScope`,
	);
	const release = parseRelease(subject.release, `${label}.release`);
	const replicaProvenance = parseReplicaProvenance(
		subject.replicaProvenance,
		deploymentScope,
	);
	const releaseArtifactProvenance = parseReleaseArtifactProvenance(
		subject.releaseArtifactProvenance,
		release,
		replicaProvenance,
	);
	const sources = requireRecord(subject.sources, `${label}.sources`);
	requireExactKeys(sources, [
		"xmppLocatorSha256",
		"prometheusLocatorSha256",
		"faroIngestLocatorSha256",
		"faroQuerySource",
	], `${label}.sources`);
	const faroQuerySource = requireRecord(
		sources.faroQuerySource,
		`${label}.sources.faroQuerySource`,
	);
	requireExactKeys(
		faroQuerySource,
		["kind", "locatorSha256", "dataSourceUidSha256"],
		`${label}.sources.faroQuerySource`,
	);
	exactLiteral(
		faroQuerySource.kind,
		"grafana-faro-query-api",
		`${label}.sources.faroQuerySource.kind`,
	);
	if (!Array.isArray(subject.artifacts)) throw new Error(`${label}.artifacts must be an array`);
	const artifacts = subject.artifacts.map((entry, index) => {
		const artifactLabel = `${label}.artifacts[${index}]`;
		const artifact = requireRecord(entry, artifactLabel);
		requireExactKeys(artifact, ["role", "sha256"], artifactLabel);
		const role = requireString(artifact, "role", artifactLabel);
		if (!LIVE_COLLECTION_ROLES.includes(role as LiveCollectionRole)) {
			throw new Error(`${artifactLabel}.role is not a live collection role`);
		}
		return {
			role: role as LiveCollectionRole,
			sha256: requireSha256(artifact.sha256, `${artifactLabel}.sha256`),
		};
	});
	if (
		JSON.stringify(artifacts.map(({ role }) => role))
		!== JSON.stringify(LIVE_COLLECTION_ROLES)
	) throw new Error(`${label}.artifacts must use the exact canonical role order`);
	return {
		schemaVersion: 1,
		evidenceKind: "gate-0-live-collection-subject",
		release,
		window: parseWindow(subject.window, `${label}.window`),
		deploymentScope,
		replicaProvenance,
		releaseArtifactProvenance,
		attestor: {
			repository: TRUSTED_REPOSITORY,
			workflow: TRUSTED_WORKFLOW,
			issuer: TRUSTED_ISSUER,
			sourceRef: TRUSTED_SOURCE_REF,
			workflowCommit,
		},
		sources: {
			xmppLocatorSha256: requireSha256(
				sources.xmppLocatorSha256,
				`${label}.sources.xmppLocatorSha256`,
			),
			prometheusLocatorSha256: requireSha256(
				sources.prometheusLocatorSha256,
				`${label}.sources.prometheusLocatorSha256`,
			),
			faroIngestLocatorSha256: requireSha256(
				sources.faroIngestLocatorSha256,
				`${label}.sources.faroIngestLocatorSha256`,
			),
			faroQuerySource: {
				kind: "grafana-faro-query-api",
				locatorSha256: requireSha256(
					faroQuerySource.locatorSha256,
					`${label}.sources.faroQuerySource.locatorSha256`,
				),
				dataSourceUidSha256: requireSha256(
					faroQuerySource.dataSourceUidSha256,
					`${label}.sources.faroQuerySource.dataSourceUidSha256`,
				),
			},
		},
		artifacts,
	};
}

export function githubAttestationArguments(
	subjectPath: string,
	bundlePath: string,
	subject: LiveCollectionSubject,
): string[] {
	return [
		"gh",
		"attestation",
		"verify",
		subjectPath,
		"--repo",
		TRUSTED_REPOSITORY,
		"--bundle",
		bundlePath,
		"--signer-workflow",
		TRUSTED_WORKFLOW,
		"--cert-identity",
		TRUSTED_WORKFLOW_IDENTITY,
		"--cert-oidc-issuer",
		TRUSTED_ISSUER,
		"--source-ref",
		TRUSTED_SOURCE_REF,
		"--source-digest",
		subject.attestor.workflowCommit,
		"--signer-digest",
		subject.attestor.workflowCommit,
		"--predicate-type",
		SLSA_PROVENANCE_V1,
		"--deny-self-hosted-runners",
		"--format",
		"json",
	];
}

async function writePinnedCopy(path: string, bytes: Uint8Array): Promise<void> {
	const handle = await open(path, "wx", 0o400);
	try {
		await handle.writeFile(bytes);
		await handle.sync();
	} finally {
		await handle.close();
	}
}

export async function withPinnedAttestationCopies<T>(
	input: {
		subjectBytes: Uint8Array;
		bundleBytes: Uint8Array;
	},
	callback: (paths: { subjectPath: string; bundlePath: string }) => Promise<T>,
): Promise<T> {
	const subjectBytes = Uint8Array.from(input.subjectBytes);
	const bundleBytes = Uint8Array.from(input.bundleBytes);
	const directory = await mkdtemp(join(tmpdir(), "waddle-gate0-attestation-"));
	await chmod(directory, 0o700);
	const subjectPath = resolve(directory, basename(LIVE_COLLECTION_SUBJECT_PATH));
	const bundlePath = resolve(directory, basename(LIVE_COLLECTION_BUNDLE_PATH));
	try {
		await writePinnedCopy(subjectPath, subjectBytes);
		await writePinnedCopy(bundlePath, bundleBytes);
		const directoryHandle = await open(directory, "r");
		try {
			await directoryHandle.sync();
		} finally {
			await directoryHandle.close();
		}
		return await callback({ subjectPath, bundlePath });
	} finally {
		await rm(directory, { recursive: true, force: true });
	}
}

export const verifyGitHubLiveCollectionAttestation: LiveCollectionAttestationVerifier =
	async ({ subject, subjectSha256, subjectBytes, bundleSha256, bundleBytes }) => {
		const pinnedSubjectBytes = Uint8Array.from(subjectBytes);
		const pinnedBundleBytes = Uint8Array.from(bundleBytes);
		if (
			createHash("sha256").update(pinnedSubjectBytes).digest("hex") !== subjectSha256
			|| createHash("sha256").update(pinnedBundleBytes).digest("hex") !== bundleSha256
		) throw new Error("live collection attestation snapshots do not match their pinned digests");
		return withPinnedAttestationCopies(
			{ subjectBytes: pinnedSubjectBytes, bundleBytes: pinnedBundleBytes },
			async ({ subjectPath, bundlePath }) => {
				const process = Bun.spawn(
					githubAttestationArguments(subjectPath, bundlePath, subject),
					{ stdout: "pipe", stderr: "pipe" },
				);
				const [exitCode, stdout] = await Promise.all([
					process.exited,
					new Response(process.stdout).text(),
					new Response(process.stderr).arrayBuffer(),
				]);
				if (exitCode !== 0) {
					throw new Error("live collection GitHub attestation verification failed closed");
				}
				const result = parseJsonDocument(stdout, "GitHub attestation verification result");
				if (!Array.isArray(result) || result.length !== 1) {
					throw new Error("GitHub attestation verification must return exactly one statement");
				}
				const expectedName = basename(subjectPath);
				const matchedSubjects = result.flatMap((entry) => {
					if (typeof entry !== "object" || entry === null) return [];
					const verification = (entry as Record<string, unknown>).verificationResult;
					if (typeof verification !== "object" || verification === null) return [];
					const statement = (verification as Record<string, unknown>).statement;
					if (typeof statement !== "object" || statement === null) return [];
					const subjects = (statement as Record<string, unknown>).subject;
					if (!Array.isArray(subjects)) return [];
					return subjects.filter((candidate) => {
						if (typeof candidate !== "object" || candidate === null) return false;
						const record = candidate as Record<string, unknown>;
						const digest = record.digest;
						return record.name === expectedName
							&& typeof digest === "object"
							&& digest !== null
							&& (digest as Record<string, unknown>).sha256 === subjectSha256;
					});
				});
				if (matchedSubjects.length !== 1) {
					throw new Error("verified GitHub attestation must bind exactly one canonical live collection subject");
				}
			},
		);
	};
