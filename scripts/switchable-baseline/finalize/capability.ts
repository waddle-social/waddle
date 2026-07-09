import { resolve } from "node:path";
import {
	type CapabilityArtifactManifest,
} from "../gate-evidence";
import {
	canonicalArtifactPaths,
	canonicalManifestPaths,
	scopesEqual,
} from "../gate-evidence/common";
import {
	validateDiscoTargetContractArtifact,
} from "../gate-evidence/capability-contract";
import { validateLiveDiscoArtifact } from "../gate-evidence/capability-live";
import {
	loadCapabilityDeclarations,
	validateReconciliationArtifact,
} from "../gate-evidence/capability-reconciliation";
import {
	readPinnedFile,
	requireRepositorySourceAtCommit,
} from "../gate-evidence/filesystem";
import { sha256Hex } from "../evidence";
import { parseJsonDocument } from "../json";
import { CAPABILITY_SERVER_SOURCE_PATHS } from "../source-contract";
import {
	capabilityArtifact,
	readRestrictedJsonInput,
	serializeCanonicalJson,
	validateFinalizationMetadata,
	type FinalizationContext,
	type GeneratedEvidence,
} from "./common";

export interface CapabilityFinalizationInput extends FinalizationContext {
	liveDiscoPath: string;
}

export async function buildCapabilityEvidence(
	input: CapabilityFinalizationInput,
): Promise<GeneratedEvidence> {
	const metadata = validateFinalizationMetadata(input);
	const sourceSnapshots = new Map(
		CAPABILITY_SERVER_SOURCE_PATHS.map((source) => [
			source,
			readPinnedFile(resolve(input.repositoryRoot, source), `capability finalizer source ${source}`),
		]),
	);
	for (const source of CAPABILITY_SERVER_SOURCE_PATHS) {
		await requireRepositorySourceAtCommit(
			input.repositoryRoot,
			metadata.release.serverCommit,
			source,
			"capability finalizer source",
			input.sourceAtCommit,
			sourceSnapshots.get(source),
		);
	}

	const contractSnapshot = sourceSnapshots.get("server/disco-target-contract.json");
	if (!contractSnapshot) throw new Error("capability target contract snapshot is missing");
	const contractContents = contractSnapshot.bytes.toString("utf8");
	const contractSha256 = contractSnapshot.sha256;
	const contractArtifact = capabilityArtifact(
		metadata,
		"disco-target-contract",
		canonicalArtifactPaths["capability-baseline"]["disco-target-contract"],
		contractSha256,
	);
	const contract = validateDiscoTargetContractArtifact(
		input.repositoryRoot,
		parseJsonDocument(contractContents, "disco target contract"),
		contractArtifact,
	);

	const liveInput = await readRestrictedJsonInput(
		input.liveDiscoPath,
		input.repositoryRoot,
		"native live disco export",
	);
	const liveArtifact = capabilityArtifact(
		metadata,
		"live-disco-export",
		canonicalArtifactPaths["capability-baseline"]["live-disco-export"],
		liveInput.sha256,
	);
	const live = validateLiveDiscoArtifact(liveInput.value, liveArtifact, contract);
	if (!scopesEqual(live.scope, metadata.deploymentScope)) {
		throw new Error("capability finalizer deployment scope must match live disco");
	}

	const capabilityManifest = sourceSnapshots.get("server/capabilities.toml");
	if (!capabilityManifest) throw new Error("capability manifest snapshot is missing");
	const capabilityManifestSha256 = capabilityManifest.sha256;
	const capabilityManifestReference = {
		path: "server/capabilities.toml",
		sha256: capabilityManifestSha256,
		schemaVersion: 1,
	};
	const declarations = loadCapabilityDeclarations(
		input.repositoryRoot,
		capabilityManifestReference,
		"capability-reconciliation artifact.capabilityManifest",
		contract,
	);
	const checks = declarations.map(({ id, targets }) => ({
		capabilityId: id,
		status: "matched" as const,
		targets: [...targets].map(([target, features]) => {
			const observed = live.featuresByTarget.get(target);
			if (!observed || features.some((feature) => !observed.has(feature))) {
				throw new Error(`live disco does not satisfy capability ${id} on ${target}`);
			}
			return {
				target,
				declaredFeatures: features,
				observedFeatures: features,
			};
		}),
	}));
	const reconciliation = {
		schemaVersion: 1,
		artifactRole: "capability-reconciliation",
		evidenceKind: "gate-0-capability-reconciliation",
		status: "matched",
		serverCommit: metadata.release.serverCommit,
		capturedAt: live.capturedAt,
		deploymentScope: metadata.deploymentScope,
		targetContractSha256: contractSha256,
		liveDiscoSha256: liveInput.sha256,
		capabilityManifest: capabilityManifestReference,
		summary: {
			declaredCapabilityCount: declarations.length,
			observedTargetCount: live.targetCount,
			missingAdvertisedFeatures: [],
			unexpectedOfficialFeatures: [],
			capabilityMismatches: [],
		},
		checks,
	};
	const reconciliationContents = serializeCanonicalJson(reconciliation);
	const reconciliationArtifact = capabilityArtifact(
		metadata,
		"capability-reconciliation",
		canonicalArtifactPaths["capability-baseline"]["capability-reconciliation"],
		sha256Hex(reconciliationContents),
	);
	validateReconciliationArtifact(
		input.repositoryRoot,
		reconciliation,
		reconciliationArtifact,
		liveArtifact,
		live,
		contract,
	);

	const manifest: CapabilityArtifactManifest = {
		schemaVersion: 1,
		evidenceKind: "capability-baseline",
		status: "complete",
		release: metadata.release,
		window: metadata.window,
		capturedAt: metadata.capturedAt,
		artifacts: [contractArtifact, liveArtifact, reconciliationArtifact],
	};
	const manifestContents = serializeCanonicalJson(manifest);
	const reference = {
		type: "artifact-manifest" as const,
		path: canonicalManifestPaths["capability-baseline"],
		sha256: sha256Hex(manifestContents),
	};
	return {
		reference,
		files: [
			{ repositoryPath: contractArtifact.path, contents: contractContents },
			{ repositoryPath: liveArtifact.path, contents: liveInput.contents },
			{
				repositoryPath: reconciliationArtifact.path,
				contents: reconciliationContents,
			},
			{ repositoryPath: reference.path, contents: manifestContents },
		],
	};
}
