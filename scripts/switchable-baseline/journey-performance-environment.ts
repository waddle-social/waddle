import {
	requireExactKeys,
	requireInteger,
	requireRecord,
	requireString,
} from "./gate-evidence/common";
import {
	readTrustedJsonSnapshot,
	requireRepositorySourceAtCommit,
	type RepositorySourceAtCommitReader,
} from "./gate-evidence/filesystem";
import type { JourneyScenarioBinding } from "./journey-evidence-reference";

export const PERFORMANCE_PROFILE_PATH = "docs/product/performance-profile.json";

function requireCommit(value: unknown, label: string): string {
	if (typeof value !== "string" || !/^[0-9a-f]{40}$/.test(value)) {
		throw new Error(`${label} must be a full lowercase Git SHA`);
	}
	return value;
}

function requireFiniteNumber(value: unknown, label: string): number {
	if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
		throw new Error(`${label} must be a non-negative finite number`);
	}
	return value;
}

export async function validatePerformanceEnvironment(
	value: unknown,
	binding: JourneyScenarioBinding,
	context: {
		repositoryRoot: string;
		contractCommit: string;
		expectedAppCommit: string;
		sourceAtCommit?: RepositorySourceAtCommitReader;
	},
): Promise<void> {
	const environment = requireRecord(value, "journey performance environment");
	const browserClient = binding.client === "desktop-web" || binding.client === "android-pwa";
	requireExactKeys(
		environment,
		[
			"profileId", "client", "hardware", "ramGiB", "network", "osVersion", "appCommit",
			...(browserClient ? ["browserVersion"] : []),
		],
		"journey performance environment",
	);
	const profileSnapshot = readTrustedJsonSnapshot(
		context.repositoryRoot,
		PERFORMANCE_PROFILE_PATH,
		"docs/product",
		"journey performance profile",
	);
	await requireRepositorySourceAtCommit(
		context.repositoryRoot,
		context.contractCommit,
		PERFORMANCE_PROFILE_PATH,
		"journey performance profile",
		context.sourceAtCommit,
		profileSnapshot,
	);
	const profile = requireRecord(profileSnapshot.value, "journey performance profile");
	requireExactKeys(
		profile,
		["schemaVersion", "id", "dataset", "network", "clients"],
		"journey performance profile",
	);
	if (profile.schemaVersion !== 1 || environment.profileId !== profile.id) {
		throw new Error("journey performance environment must bind the canonical profile id");
	}
	if (environment.client !== binding.client) {
		throw new Error("journey performance environment client must match its scenario");
	}
	const clients = requireRecord(profile.clients, "journey performance profile.clients");
	const expectedClient = requireRecord(
		clients[binding.client],
		`journey performance profile.clients.${binding.client}`,
	);
	requireExactKeys(
		expectedClient,
		["hardware", "ramGiB", "resolvedRuntimeRequired"],
		`journey performance profile.clients.${binding.client}`,
	);
	if (
		environment.hardware !== expectedClient.hardware
		|| environment.ramGiB !== expectedClient.ramGiB
		|| expectedClient.resolvedRuntimeRequired !== true
	) throw new Error("journey performance environment does not match its resolved client profile");
	const expectedNetwork = requireRecord(profile.network, "journey performance profile.network");
	requireExactKeys(expectedNetwork, ["downMbps", "upMbps", "roundTripMs"], "journey performance profile.network");
	const actualNetwork = requireRecord(environment.network, "journey performance environment.network");
	requireExactKeys(actualNetwork, ["downMbps", "upMbps", "roundTripMs"], "journey performance environment.network");
	for (const key of ["downMbps", "upMbps", "roundTripMs"] as const) {
		if (
			requireFiniteNumber(actualNetwork[key], `journey performance environment.network.${key}`)
			!== requireFiniteNumber(expectedNetwork[key], `journey performance profile.network.${key}`)
		) throw new Error("journey performance environment network does not match its profile");
	}
	const osVersion = requireString(environment, "osVersion", "journey performance environment");
	if (!/^[A-Za-z0-9][A-Za-z0-9_. -]+$/.test(osVersion) || osVersion.length > 128) {
		throw new Error("journey performance environment OS version is not bounded");
	}
	if (
		requireCommit(environment.appCommit, "journey performance environment.appCommit")
		!== context.expectedAppCommit
	) throw new Error("journey performance environment app commit does not match its client release");
	if (browserClient) {
		const browserVersion = requireString(
			environment,
			"browserVersion",
			"journey performance environment",
		);
		if (!/^[A-Za-z]+\/[0-9]+(?:\.[0-9]+){1,3}$/.test(browserVersion)) {
			throw new Error("journey performance environment browser version is invalid");
		}
	}
	requireInteger(expectedClient.ramGiB, "journey performance profile client RAM", 1);
}
