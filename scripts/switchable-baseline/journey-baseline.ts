import { createHash } from "node:crypto";
import { resolve } from "node:path";
import {
	CRITICAL_JOURNEY_CONTRACT_PATH,
	CRITICAL_JOURNEY_VALIDATOR_PATH,
	validateCriticalJourneyContract,
	type CriticalJourneySummary,
} from "./critical-journey-contract";
import {
	requireExactKeys,
	requireInteger,
	requireRecord,
	requireSha256,
	requireString,
} from "./gate-evidence/common";
import {
	readPinnedFile,
	readTrustedJsonSnapshot,
	requireRepositorySourceAtCommit,
	resolveTrustedRepositoryFile,
	type RepositorySourceAtCommitReader,
} from "./gate-evidence/filesystem";
import { commitFilesNoClobber } from "./no-clobber";

export const JOURNEY_BASELINE_MANIFEST_PATH =
	"docs/evidence/journey-baseline.manifest.json";
export const CRITICAL_JOURNEY_VALIDATOR_PATHS = [
	"scripts/switchable-baseline/journey-baseline.ts",
	CRITICAL_JOURNEY_VALIDATOR_PATH,
	"scripts/switchable-baseline/journey-evidence-reference.ts",
	"scripts/switchable-baseline/journey-performance-environment.ts",
	"scripts/switchable-baseline/gate-evidence/common.ts",
	"scripts/switchable-baseline/gate-evidence/filesystem.ts",
	"scripts/switchable-baseline/json.ts",
	"scripts/switchable-baseline/model.ts",
] as const;

export interface JourneyBaselineManifestReference {
	type: "journey-baseline-manifest";
	path: typeof JOURNEY_BASELINE_MANIFEST_PATH;
	sha256: string;
}

export async function buildJourneyBaselineManifest(
	repositoryRoot: string,
	commit: string,
	sourceAtCommit?: RepositorySourceAtCommitReader,
): Promise<JourneyBaselineManifestReference> {
	const releaseCommit = requireCommit(commit, "journey-baseline release commit");
	const release = {
		contractCommit: releaseCommit,
		serverCommit: releaseCommit,
		webCommit: releaseCommit,
		clientCommits: {
			"desktop-web": releaseCommit,
			"android-pwa": releaseCommit,
			ios: releaseCommit,
			macos: releaseCommit,
		},
	} as const;
	const contractSnapshot = readTrustedJsonSnapshot(
		repositoryRoot,
		CRITICAL_JOURNEY_CONTRACT_PATH,
		"docs/product",
		"journey-baseline contract",
	);
	const validatorSnapshots = CRITICAL_JOURNEY_VALIDATOR_PATHS.map((path) => ({
		path,
		snapshot: readPinnedFile(
			resolveTrustedRepositoryFile(
				repositoryRoot,
				path,
				"scripts",
				"journey-baseline validator",
			),
			"journey-baseline validator",
		),
	}));
	for (const { path, snapshot } of [
		{ path: CRITICAL_JOURNEY_CONTRACT_PATH, snapshot: contractSnapshot },
		...validatorSnapshots,
	]) {
		await requireRepositorySourceAtCommit(
			repositoryRoot,
			releaseCommit,
			path,
			"journey-baseline source",
			sourceAtCommit,
			snapshot,
		);
	}
	const manifest = {
		schemaVersion: 1,
		evidenceKind: "journey-baseline",
		status: "complete",
		release,
		contract: {
			path: CRITICAL_JOURNEY_CONTRACT_PATH,
			sha256: contractSnapshot.sha256,
			commit: releaseCommit,
		},
		validator: {
			commit: releaseCommit,
			sources: validatorSnapshots.map(({ path, snapshot }) => ({
				path,
				sha256: snapshot.sha256,
			})),
		},
		summary: await validateCriticalJourneyContract(contractSnapshot.value, {
			repositoryRoot,
			release,
			sourceAtCommit,
		}),
	} as const;
	const contents = `${JSON.stringify(manifest, null, 2)}\n`;
	const reference: JourneyBaselineManifestReference = {
		type: "journey-baseline-manifest",
		path: JOURNEY_BASELINE_MANIFEST_PATH,
		sha256: createHash("sha256").update(contents).digest("hex"),
	};
	await commitFilesNoClobber(
		repositoryRoot,
		[{
			path: resolve(repositoryRoot, JOURNEY_BASELINE_MANIFEST_PATH),
			contents,
		}],
		async () => validateJourneyBaselineManifestReference(
			repositoryRoot,
			reference,
			sourceAtCommit,
		),
	);
	return reference;
}

function requireCommit(value: unknown, label: string): string {
	if (typeof value !== "string" || !/^[0-9a-f]{40}$/.test(value)) {
		throw new Error(`${label} must be a full lowercase Git SHA`);
	}
	return value;
}

function parseJourneyRelease(value: unknown): {
	contractCommit: string;
	serverCommit: string;
	webCommit: string;
	clientCommits: Record<"desktop-web" | "android-pwa" | "ios" | "macos", string>;
} {
	const release = requireRecord(value, "journey-baseline manifest.release");
	requireExactKeys(
		release,
		["contractCommit", "serverCommit", "webCommit", "clientCommits"],
		"journey-baseline manifest.release",
	);
	const clientCommits = requireRecord(
		release.clientCommits,
		"journey-baseline manifest.release.clientCommits",
	);
	requireExactKeys(
		clientCommits,
		["desktop-web", "android-pwa", "ios", "macos"],
		"journey-baseline manifest.release.clientCommits",
	);
	const result = {
		contractCommit: requireCommit(
			release.contractCommit,
			"journey-baseline manifest.release.contractCommit",
		),
		serverCommit: requireCommit(
			release.serverCommit,
			"journey-baseline manifest.release.serverCommit",
		),
		webCommit: requireCommit(
			release.webCommit,
			"journey-baseline manifest.release.webCommit",
		),
		clientCommits: {
			"desktop-web": requireCommit(clientCommits["desktop-web"], "desktop-web client commit"),
			"android-pwa": requireCommit(clientCommits["android-pwa"], "android-pwa client commit"),
			ios: requireCommit(clientCommits.ios, "ios client commit"),
			macos: requireCommit(clientCommits.macos, "macos client commit"),
		},
	};
	if (
		result.clientCommits["desktop-web"] !== result.webCommit
		|| result.clientCommits["android-pwa"] !== result.webCommit
	) throw new Error("journey-baseline browser client commits must match webCommit");
	return result;
}

function validateManifestSummary(value: unknown): CriticalJourneySummary {
	const summary = requireRecord(value, "journey-baseline manifest.summary");
	requireExactKeys(summary, [
		"journeyCount",
		"requirementCount",
		"scenarioCount",
		"journeyStatus",
		"gateReadiness",
		"matrixSha256",
	], "journey-baseline manifest.summary");
	const gateReadiness = requireRecord(
		summary.gateReadiness,
		"journey-baseline manifest.summary.gateReadiness",
	);
	requireExactKeys(
		gateReadiness,
		["2", "3", "4"],
		"journey-baseline manifest.summary.gateReadiness",
	);
	for (const gate of ["2", "3", "4"] as const) {
		if (gateReadiness[gate] !== "ready" && gateReadiness[gate] !== "not-ready") {
			throw new Error(`journey-baseline manifest.summary.gateReadiness.${gate} is invalid`);
		}
	}
	if (summary.journeyStatus !== "ready" && summary.journeyStatus !== "not-ready") {
		throw new Error("journey-baseline manifest.summary.journeyStatus is invalid");
	}
	return {
		journeyCount: requireInteger(summary.journeyCount, "journey-baseline manifest.summary.journeyCount", 1),
		requirementCount: requireInteger(
			summary.requirementCount,
			"journey-baseline manifest.summary.requirementCount",
			1,
		),
		scenarioCount: requireInteger(summary.scenarioCount, "journey-baseline manifest.summary.scenarioCount", 100),
		journeyStatus: summary.journeyStatus,
		gateReadiness: {
			"2": gateReadiness["2"] as "ready" | "not-ready",
			"3": gateReadiness["3"] as "ready" | "not-ready",
			"4": gateReadiness["4"] as "ready" | "not-ready",
		},
		matrixSha256: requireSha256(
			summary.matrixSha256,
			"journey-baseline manifest.summary.matrixSha256",
		),
	};
}

export async function validateJourneyBaselineManifestReference(
	repositoryRoot: string,
	value: unknown,
	sourceAtCommit?: RepositorySourceAtCommitReader,
): Promise<void> {
	const reference = requireRecord(value, "journey-baseline reference");
	requireExactKeys(reference, ["type", "path", "sha256"], "journey-baseline reference");
	if (
		reference.type !== "journey-baseline-manifest"
		|| reference.path !== JOURNEY_BASELINE_MANIFEST_PATH
	) {
		throw new Error("journey-baseline completion must use the canonical typed manifest");
	}
	const expectedManifestSha = requireSha256(
		reference.sha256,
		"journey-baseline reference.sha256",
	);
	const snapshot = readTrustedJsonSnapshot(
		repositoryRoot,
		JOURNEY_BASELINE_MANIFEST_PATH,
		"docs/evidence",
		"journey-baseline manifest",
		".json",
		expectedManifestSha,
	);
	const manifest = requireRecord(snapshot.value, "journey-baseline manifest");
	requireExactKeys(manifest, [
		"schemaVersion",
		"evidenceKind",
		"status",
		"release",
		"contract",
		"validator",
		"summary",
	], "journey-baseline manifest");
	if (
		manifest.schemaVersion !== 1
		|| manifest.evidenceKind !== "journey-baseline"
		|| manifest.status !== "complete"
	) throw new Error("journey-baseline manifest must be complete schema version 1");

	const release = parseJourneyRelease(manifest.release);
	const commit = release.contractCommit;
	const contract = requireRecord(manifest.contract, "journey-baseline manifest.contract");
	requireExactKeys(contract, ["path", "sha256", "commit"], "journey-baseline manifest.contract");
	if (contract.path !== CRITICAL_JOURNEY_CONTRACT_PATH || contract.commit !== commit) {
		throw new Error("journey-baseline manifest contract must bind the canonical release source");
	}
	const contractSha256 = requireSha256(
		contract.sha256,
		"journey-baseline manifest.contract.sha256",
	);
	const validator = requireRecord(manifest.validator, "journey-baseline manifest.validator");
	requireExactKeys(validator, ["sources", "commit"], "journey-baseline manifest.validator");
	if (validator.commit !== commit || !Array.isArray(validator.sources)) {
		throw new Error("journey-baseline manifest validator must bind the canonical release source");
	}
	const validatorSources = validator.sources.map((value, index) => {
		const source = requireRecord(value, `journey-baseline manifest.validator.sources[${index}]`);
		requireExactKeys(
			source,
			["path", "sha256"],
			`journey-baseline manifest.validator.sources[${index}]`,
		);
		return {
			path: requireString(source, "path", `journey-baseline manifest.validator.sources[${index}]`),
			sha256: requireSha256(
				source.sha256,
				`journey-baseline manifest.validator.sources[${index}].sha256`,
			),
		};
	});
	if (
		JSON.stringify(validatorSources.map(({ path }) => path))
		!== JSON.stringify(CRITICAL_JOURNEY_VALIDATOR_PATHS)
	) throw new Error("journey-baseline manifest must bind every canonical validator source");

	const trustedContract = readTrustedJsonSnapshot(
		repositoryRoot,
		CRITICAL_JOURNEY_CONTRACT_PATH,
		"docs/product",
		"journey-baseline contract",
		".json",
		contractSha256,
	);
	const validatorSnapshots = validatorSources.map(({ path, sha256 }) => {
		const snapshot = readPinnedFile(
			resolveTrustedRepositoryFile(
				repositoryRoot,
				path,
				"scripts",
				"journey-baseline validator",
			),
			"journey-baseline validator",
		);
		if (snapshot.sha256 !== sha256) {
			throw new Error("journey-baseline validator digest does not match its bytes");
		}
		return { path, snapshot };
	});
	for (const { path, snapshot: sourceSnapshot } of [
		{ path: CRITICAL_JOURNEY_CONTRACT_PATH, snapshot: trustedContract },
		...validatorSnapshots,
	]) {
		await requireRepositorySourceAtCommit(
			repositoryRoot,
			commit,
			path,
			"journey-baseline source",
			sourceAtCommit,
			sourceSnapshot,
		);
	}

	const expectedSummary = await validateCriticalJourneyContract(trustedContract.value, {
		repositoryRoot,
		release,
		sourceAtCommit,
	});
	const declaredSummary = validateManifestSummary(manifest.summary);
	if (JSON.stringify(declaredSummary) !== JSON.stringify(expectedSummary)) {
		throw new Error("journey-baseline manifest summary does not match deterministic validation");
	}
}
