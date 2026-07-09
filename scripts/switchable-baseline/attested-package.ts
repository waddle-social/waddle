import { resolve } from "node:path";
import {
	LIVE_COLLECTION_BUNDLE_PATH,
	LIVE_COLLECTION_ROLES,
	LIVE_COLLECTION_SUBJECT_PATH,
	parseLiveCollectionSubject,
	verifyGitHubLiveCollectionAttestation,
	type LiveCollectionAttestationVerifier,
} from "./attestation";
import {
	canonicalGateZeroPaths,
	releasesEqual,
	sameWindow,
	scopesEqual,
	type ValidatedGateZeroArtifactManifest,
} from "./gate-evidence/common";
import {
	readPinnedFile,
	readTrustedJsonSnapshot,
	requirePinnedFileUnchanged,
	requireSealedGateZeroEvidenceDirectory,
	resolveTrustedEvidenceFile,
} from "./gate-evidence/filesystem";
import {
	verifyTrustedReleaseArtifactProvenance,
	type ReleaseArtifactProvenanceVerifier,
} from "./release-artifact-provenance";

export async function verifyAttestedGateZeroPackage(
	repositoryRoot: string,
	manifests: readonly ValidatedGateZeroArtifactManifest[],
	verifier: LiveCollectionAttestationVerifier = verifyGitHubLiveCollectionAttestation,
	releaseArtifactVerifier: ReleaseArtifactProvenanceVerifier =
		verifyTrustedReleaseArtifactProvenance,
): Promise<void> {
	const packageSnapshots = new Map(canonicalGateZeroPaths.map((path) => [
		path,
		readPinnedFile(
			resolve(repositoryRoot, path),
			`Gate 0 attested package ${path}`,
		),
	]));
	const capability = manifests.find(({ evidenceKind }) => evidenceKind === "capability-baseline");
	const telemetry = manifests.find(({ evidenceKind }) => evidenceKind === "telemetry-baseline");
	if (!capability || !telemetry || manifests.length !== 2) {
		throw new Error("attested Gate 0 package requires exactly capability and telemetry manifests");
	}
	const subjectSnapshot = readTrustedJsonSnapshot(
		repositoryRoot,
		LIVE_COLLECTION_SUBJECT_PATH,
		"docs/evidence",
		"live collection subject",
	);
	const bundlePath = resolveTrustedEvidenceFile(
		repositoryRoot,
		LIVE_COLLECTION_BUNDLE_PATH,
		"live collection Sigstore bundle",
		".json",
	);
	const bundleSnapshot = readPinnedFile(bundlePath, "live collection Sigstore bundle");
	const subject = parseLiveCollectionSubject(subjectSnapshot.value);
	if (!releasesEqual(subject.release, capability.release)) {
		throw new Error("live collection subject release must match both Gate 0 manifests");
	}
	if (!sameWindow(subject.window, capability.window)) {
		throw new Error("live collection subject window must match both Gate 0 manifests");
	}
	if (!scopesEqual(subject.deploymentScope, capability.deploymentScope)) {
		throw new Error("live collection subject scope must match both Gate 0 manifests");
	}
	const artifacts = new Map(
		[...capability.artifacts, ...telemetry.artifacts].map(({ role, sha256 }) => [role, sha256]),
	);
	for (const entry of subject.artifacts) {
		if (artifacts.get(entry.role) !== entry.sha256) {
			throw new Error(`live collection subject digest does not match ${entry.role}`);
		}
	}
	if (subject.artifacts.length !== LIVE_COLLECTION_ROLES.length) {
		throw new Error("live collection subject must bind every live Gate 0 artifact");
	}
	await verifier({
		subjectPath: subjectSnapshot.path,
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
	requirePinnedFileUnchanged(
		subjectSnapshot.path,
		subjectSnapshot,
		"live collection subject",
	);
	requirePinnedFileUnchanged(
		bundlePath,
		bundleSnapshot,
		"live collection Sigstore bundle",
	);
	for (const [path, snapshot] of packageSnapshots) {
		requirePinnedFileUnchanged(
			resolve(repositoryRoot, path),
			snapshot,
			`Gate 0 attested package ${path}`,
		);
	}
	requireSealedGateZeroEvidenceDirectory(repositoryRoot, [
		LIVE_COLLECTION_SUBJECT_PATH,
		LIVE_COLLECTION_BUNDLE_PATH,
	]);
}
