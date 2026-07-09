import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import {
	LIVE_COLLECTION_BUNDLE_PATH,
	LIVE_COLLECTION_SUBJECT_PATH,
	parseLiveCollectionSubject,
	verifyGitHubLiveCollectionAttestation,
	type LiveCollectionAttestationVerifier,
	type LiveCollectionRole,
} from "../attestation";
import {
	releasesEqual,
	sameWindow,
	scopesEqual,
} from "../gate-evidence/common";
import {
	readPinnedFile,
	requirePinnedFileUnchanged,
	readRepositorySourceAtCommit,
	type RepositorySourceAtCommitReader,
} from "../gate-evidence/filesystem";
import {
	activateGateZeroGeneration,
	cleanupGateZeroGeneration,
	createGateZeroGeneration,
} from "../generation";
import { resolveRestrictedEvidenceInput } from "../filesystem";
import { parseJsonDocument } from "../json";
import { verifyGateZeroEvidencePackage } from "../finalize";
import { buildCapabilityEvidence } from "./capability";
import {
	readRestrictedJsonInput,
	validateFinalizationMetadata,
	type FinalizationContext,
} from "./common";
import { buildTelemetryEvidence, type TelemetryFinalizationInput } from "./telemetry";
import { GATE_ZERO_SOURCE_PATHS } from "../source-contract";
import {
	verifyTrustedReleaseArtifactProvenance,
	type ReleaseArtifactProvenanceVerifier,
} from "../release-artifact-provenance";

type FaroPaths = TelemetryFinalizationInput["faroPaths"];

export interface GateZeroFinalizationInput extends FinalizationContext {
	liveDiscoPath: string;
	prometheusPath: string;
	faroPaths: FaroPaths;
	subjectPath: string;
	bundlePath: string;
	attestationVerifier?: LiveCollectionAttestationVerifier;
	releaseArtifactVerifier?: ReleaseArtifactProvenanceVerifier;
}

async function writeNew(path: string, bytes: string | Uint8Array): Promise<void> {
	await mkdir(dirname(path), { recursive: true });
	await writeFile(path, bytes, { flag: "wx", mode: 0o644 });
}

function expectedStagingPath(repositoryRoot: string, suffix: string): string {
	return resolve(repositoryRoot, "target/switchable-baseline-inputs", suffix);
}

export async function finalizeGateZeroEvidence(
	input: GateZeroFinalizationInput,
): Promise<void> {
	const metadata = validateFinalizationMetadata(input);
	const expectedSubject = expectedStagingPath(
		input.repositoryRoot,
		"attestation/live-collection-subject.json",
	);
	const expectedBundle = expectedStagingPath(
		input.repositoryRoot,
		"attestation/live-collection.sigstore.json",
	);
	if (resolve(input.subjectPath) !== expectedSubject || resolve(input.bundlePath) !== expectedBundle) {
		throw new Error("Gate 0 finalizer requires the canonical attested staging paths");
	}
	const subjectPath = await resolveRestrictedEvidenceInput(
		input.subjectPath,
		input.repositoryRoot,
		"live collection subject",
	);
	const bundlePath = await resolveRestrictedEvidenceInput(
		input.bundlePath,
		input.repositoryRoot,
		"live collection Sigstore bundle",
	);
	const subjectSnapshot = readPinnedFile(subjectPath, "live collection subject");
	const bundleSnapshot = readPinnedFile(bundlePath, "live collection Sigstore bundle");
	const subject = parseLiveCollectionSubject(parseJsonDocument(
		subjectSnapshot.bytes.toString("utf8"),
		"live collection subject",
	));
	if (!releasesEqual(subject.release, metadata.release)) {
		throw new Error("attested live collection release does not match finalization metadata");
	}
	if (!sameWindow(subject.window, metadata.window)) {
		throw new Error("attested live collection window does not match finalization metadata");
	}
	if (!scopesEqual(subject.deploymentScope, metadata.deploymentScope)) {
		throw new Error("attested live collection scope does not match finalization metadata");
	}

	const stagedByRole: Record<LiveCollectionRole, string> = {
		"live-disco-export": input.liveDiscoPath,
		"prometheus-baseline": input.prometheusPath,
		"faro-browser-auth-bootstrap": input.faroPaths["faro-browser-auth-bootstrap"],
		"faro-browser-message-ack-latency": input.faroPaths["faro-browser-message-ack-latency"],
		"faro-browser-session-lifecycle": input.faroPaths["faro-browser-session-lifecycle"],
		"faro-browser-reconnect-duration": input.faroPaths["faro-browser-reconnect-duration"],
	};
	const staged = new Map<LiveCollectionRole, Awaited<ReturnType<typeof readRestrictedJsonInput>>>();
	for (const artifact of subject.artifacts) {
		const snapshot = await readRestrictedJsonInput(
			stagedByRole[artifact.role],
			input.repositoryRoot,
			`attested ${artifact.role}`,
		);
		if (snapshot.sha256 !== artifact.sha256) {
			throw new Error(`attested digest does not match staged ${artifact.role}`);
		}
		staged.set(artifact.role, snapshot);
	}
	const verifier = input.attestationVerifier ?? verifyGitHubLiveCollectionAttestation;
	const releaseArtifactVerifier = input.releaseArtifactVerifier
		?? verifyTrustedReleaseArtifactProvenance;
	await verifier({
		subjectPath,
		bundlePath,
		subject,
		subjectSha256: subjectSnapshot.sha256,
		subjectBytes: subjectSnapshot.bytes,
		bundleSha256: bundleSnapshot.sha256,
		bundleBytes: bundleSnapshot.bytes,
	});
	await releaseArtifactVerifier({
		provenance: subject.releaseArtifactProvenance,
		release: subject.release,
	});
	requirePinnedFileUnchanged(subjectPath, subjectSnapshot, "live collection subject");
	requirePinnedFileUnchanged(bundlePath, bundleSnapshot, "live collection Sigstore bundle");
	const capabilityEvidence = await buildCapabilityEvidence({
		repositoryRoot: input.repositoryRoot,
		sourceAtCommit: input.sourceAtCommit,
		...metadata,
		liveDiscoPath: input.liveDiscoPath,
	});
	const telemetryEvidence = await buildTelemetryEvidence({
		repositoryRoot: input.repositoryRoot,
		sourceAtCommit: input.sourceAtCommit,
		...metadata,
		prometheusPath: input.prometheusPath,
		faroPaths: input.faroPaths,
	});
	const generatedFiles = [
		...capabilityEvidence.files,
		...telemetryEvidence.files,
	];
	if (
		new Set(generatedFiles.map(({ repositoryPath }) => repositoryPath)).size
		!== generatedFiles.length
	) throw new Error("Gate 0 builders produced duplicate canonical output paths");

	const generation = await createGateZeroGeneration(input.repositoryRoot);
	try {
		for (const sourcePath of GATE_ZERO_SOURCE_PATHS) {
			const source = readPinnedFile(
				resolve(input.repositoryRoot, sourcePath),
				`Gate 0 source ${sourcePath}`,
			);
			await writeNew(resolve(generation.repositoryRoot, sourcePath), source.bytes);
		}
		for (const file of generatedFiles) {
			await writeNew(
				resolve(generation.repositoryRoot, file.repositoryPath),
				file.contents,
			);
		}
		const sourceReader: RepositorySourceAtCommitReader = (
			_repositoryRoot,
			commit,
			repositoryPath,
		) => (input.sourceAtCommit ?? readRepositorySourceAtCommit)(
			input.repositoryRoot,
			commit,
			repositoryPath,
		);
		await writeNew(
			resolve(generation.repositoryRoot, LIVE_COLLECTION_SUBJECT_PATH),
			subjectSnapshot.bytes,
		);
		await writeNew(
			resolve(generation.repositoryRoot, LIVE_COLLECTION_BUNDLE_PATH),
			bundleSnapshot.bytes,
		);
		await verifyGateZeroEvidencePackage(
			generation.repositoryRoot,
			sourceReader,
			verifier,
			releaseArtifactVerifier,
		);
		await activateGateZeroGeneration({
			actualRepositoryRoot: input.repositoryRoot,
			generation,
			commitRelativePath: "attestations/live-collection.sigstore.json",
			validateBeforeCommit: async () => {
				await verifyGateZeroEvidencePackage(
					generation.repositoryRoot,
					sourceReader,
					verifier,
					releaseArtifactVerifier,
				);
			},
		});
	} finally {
		await cleanupGateZeroGeneration(generation);
	}
}
