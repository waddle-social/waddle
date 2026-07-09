import { createHash } from "node:crypto";
import {
	validateJourneyEvidenceReference,
	type JourneyEvidenceRelease,
	type JourneyGateId,
	type JourneyScenarioBinding,
} from "./journey-evidence-reference";
import { validatePerformanceEnvironment } from "./journey-performance-environment";
import type { RepositorySourceAtCommitReader } from "./gate-evidence/filesystem";
import { compareText } from "./model";

export const CRITICAL_JOURNEY_CONTRACT_PATH = "docs/product/critical-journeys.json";
export const CRITICAL_JOURNEY_VALIDATOR_PATH =
	"scripts/switchable-baseline/critical-journey-contract.ts";

export const immutableJourneyContract = {
	authenticate: { owner: "identity", gate: 3, requiredKinds: ["e2e"] },
	"invite-and-join": { owner: "community-onboarding", gate: 4, requiredKinds: ["e2e", "metric"] },
	"room-messaging": { owner: "messaging", gate: 3, requiredKinds: ["e2e"] },
	"direct-messaging": { owner: "messaging", gate: 3, requiredKinds: ["e2e"] },
	history: { owner: "messaging-history", gate: 3, requiredKinds: ["e2e"] },
	"unread-state": { owner: "inbox", gate: 3, requiredKinds: ["e2e"] },
	notifications: { owner: "notifications", gate: 3, requiredKinds: ["e2e"] },
	search: { owner: "search", gate: 3, requiredKinds: ["e2e"] },
	"file-sharing": { owner: "files", gate: 3, requiredKinds: ["e2e"] },
	"threads-and-replies": { owner: "threads", gate: 3, requiredKinds: ["e2e"] },
	reactions: { owner: "messaging", gate: 3, requiredKinds: ["e2e"] },
	moderation: {
		owner: "trust-and-safety",
		gate: 4,
		requiredKinds: ["e2e", "authorization", "audit", "metric"],
	},
	calls: { owner: "calls", gate: 3, requiredKinds: ["e2e"] },
	reconnect: { owner: "connection-reliability", gate: 3, requiredKinds: ["e2e", "chaos"] },
	"multi-device": { owner: "multi-device", gate: 3, requiredKinds: ["e2e"] },
	accessibility: { owner: "design-system", gate: 3, requiredKinds: ["e2e", "accessibility"] },
	"keyboard-navigation": {
		owner: "design-system",
		gate: 3,
		requiredKinds: ["e2e", "accessibility"],
	},
	"performance-budgets": { owner: "web-platform", gate: 3, requiredKinds: ["e2e", "performance"] },
	"pwa-lifecycle": { owner: "web-platform", gate: 3, requiredKinds: ["e2e", "device"] },
	"member-lifecycle-and-administration": {
		owner: "community-administration",
		gate: 4,
		requiredKinds: ["e2e", "authorization", "audit", "metric"],
	},
	"admin-operational-health": {
		owner: "community-administration",
		gate: 4,
		requiredKinds: ["e2e", "authorization", "metric"],
	},
	"federation-interoperability": {
		owner: "federation",
		gate: 2,
		requiredKinds: ["e2e", "interop", "security"],
	},
	"federation-failure-isolation": {
		owner: "federation",
		gate: 2,
		requiredKinds: ["e2e", "chaos", "security"],
	},
} as const;

const expectedClients = ["desktop-web", "android-pwa", "ios", "macos"] as const;
const expectedTopologies = ["local", "federated"] as const;
const gateIds = ["2", "3", "4"] as const;
const boundedId = /^[a-z][a-z0-9-]*$/;
const xepId = /^XEP-[0-9]{4}$/;

type JourneyId = keyof typeof immutableJourneyContract;
type EvidenceStatus = "missing" | "partial" | "complete";
type Readiness = "not-ready" | "ready";

export interface CriticalJourneySummary {
	journeyCount: number;
	requirementCount: number;
	scenarioCount: number;
	journeyStatus: Readiness;
	gateReadiness: Record<(typeof gateIds)[number], Readiness>;
	matrixSha256: string;
}

export interface CriticalJourneyValidationContext {
	repositoryRoot: string;
	release: {
		contractCommit: string;
		serverCommit: string;
		webCommit: string;
		clientCommits: Record<(typeof expectedClients)[number], string>;
	};
	sourceAtCommit?: RepositorySourceAtCommitReader;
}

function record(value: unknown, label: string): Record<string, unknown> {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		throw new Error(`${label} must be an object`);
	}
	return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[], label: string): void {
	if (JSON.stringify(Object.keys(value).sort()) !== JSON.stringify([...keys].sort())) {
		throw new Error(`${label} has an unsupported shape`);
	}
}

function nonEmptyString(value: unknown, label: string): string {
	if (typeof value !== "string" || value.trim().length === 0) {
		throw new Error(`${label} must be a non-empty string`);
	}
	return value;
}

function uniqueStrings(value: unknown, label: string): string[] {
	if (!Array.isArray(value) || value.length === 0) {
		throw new Error(`${label} must be a non-empty array`);
	}
	const values = value.map((entry, index) => nonEmptyString(entry, `${label}[${index}]`));
	if (new Set(values).size !== values.length) throw new Error(`${label} must be unique`);
	return values;
}

function exactSet(actual: readonly string[], expected: readonly string[], label: string): void {
	if (JSON.stringify([...actual].sort()) !== JSON.stringify([...expected].sort())) {
		throw new Error(`${label} must match the immutable program contract`);
	}
}

function status(value: unknown, label: string): EvidenceStatus {
	if (value !== "missing" && value !== "partial" && value !== "complete") {
		throw new Error(`${label} must be missing, partial, or complete`);
	}
	return value;
}

function readiness(value: unknown, label: string): Readiness {
	if (value !== "not-ready" && value !== "ready") {
		throw new Error(`${label} must be not-ready or ready`);
	}
	return value;
}

function deriveStatus(
	scenarioIds: readonly string[],
	requiredKinds: readonly string[],
	records: ReadonlyMap<string, Exclude<EvidenceStatus, "missing">>,
): EvidenceStatus {
	if (records.size === 0) return "missing";
	for (const scenarioId of scenarioIds) {
		for (const kind of requiredKinds) {
			if (records.get(`${scenarioId}\0${kind}`) !== "complete") return "partial";
		}
	}
	return "complete";
}

export async function validateCriticalJourneyContract(
	value: unknown,
	context?: CriticalJourneyValidationContext,
): Promise<CriticalJourneySummary> {
	const document = record(value, "critical journey contract");
	exactKeys(document, [
		"schemaVersion",
		"milestone",
		"journeyStatus",
		"gateReadiness",
		"performanceProfile",
		"releasePolicy",
		"dimensions",
		"journeys",
	], "critical journey contract");
	if (document.schemaVersion !== 1 || document.milestone !== "switchable-alternative") {
		throw new Error("critical journey contract must use switchable-alternative schema version 1");
	}
	if (document.performanceProfile !== "docs/product/performance-profile.json") {
		throw new Error("critical journey contract must use the canonical performance profile");
	}
	nonEmptyString(document.releasePolicy, "critical journey contract.releasePolicy");
	const dimensions = record(document.dimensions, "critical journey contract.dimensions");
	exactKeys(dimensions, ["clients", "topologies"], "critical journey contract.dimensions");
	exactSet(uniqueStrings(dimensions.clients, "critical journey contract.dimensions.clients"), expectedClients, "clients");
	exactSet(
		uniqueStrings(dimensions.topologies, "critical journey contract.dimensions.topologies"),
		expectedTopologies,
		"topologies",
	);
	const declaredGateReadiness = record(document.gateReadiness, "critical journey contract.gateReadiness");
	exactKeys(declaredGateReadiness, gateIds, "critical journey contract.gateReadiness");
	for (const gate of gateIds) readiness(declaredGateReadiness[gate], `gate ${gate} readiness`);
	readiness(document.journeyStatus, "critical journey contract.journeyStatus");

	if (!Array.isArray(document.journeys)) throw new Error("critical journey contract.journeys must be an array");
	const journeys = document.journeys;
	const seenJourneys = new Set<string>();
	const journeyStatuses = new Map<JourneyId, EvidenceStatus>();
	const matrix: Array<{
		journeyId: JourneyId;
		owner: string;
		gate: number;
		scenarioId: string;
		client: string;
		topology: string;
		evidence: Array<{ kind: string; status: EvidenceStatus }>;
	}> = [];
	let requirementCount = 0;

	for (const [journeyIndex, journeyValue] of journeys.entries()) {
		const label = `critical journey contract.journeys[${journeyIndex}]`;
		const journey = record(journeyValue, label);
		exactKeys(journey, [
			"id", "title", "owner", "gate", "evidence", "clients", "topologies", "protocols", "requirements",
		], label);
		const id = nonEmptyString(journey.id, `${label}.id`);
		if (!boundedId.test(id) || !(id in immutableJourneyContract) || seenJourneys.has(id)) {
			throw new Error(`${label}.id must be one unique immutable journey id`);
		}
		seenJourneys.add(id);
		const journeyId = id as JourneyId;
		const expected = immutableJourneyContract[journeyId];
		nonEmptyString(journey.title, `${label}.title`);
		if (journey.owner !== expected.owner || journey.gate !== expected.gate) {
			throw new Error(`${label} owner and gate must match the immutable program contract`);
		}
		const clients = uniqueStrings(journey.clients, `${label}.clients`);
		for (const client of clients) {
			if (!expectedClients.includes(client as (typeof expectedClients)[number])) {
				throw new Error(`${label}.clients contains an unsupported client`);
			}
		}
		const topologies = uniqueStrings(journey.topologies, `${label}.topologies`);
		for (const topology of topologies) {
			if (!expectedTopologies.includes(topology as (typeof expectedTopologies)[number])) {
				throw new Error(`${label}.topologies contains an unsupported topology`);
			}
		}
		const protocols = Array.isArray(journey.protocols)
			? journey.protocols.map((entry, index) => nonEmptyString(entry, `${label}.protocols[${index}]`))
			: undefined;
		if (!protocols || new Set(protocols).size !== protocols.length || protocols.some((entry) => !xepId.test(entry))) {
			throw new Error(`${label}.protocols must contain unique XEP identifiers`);
		}

		if (!Array.isArray(journey.requirements) || journey.requirements.length === 0) {
			throw new Error(`${label}.requirements must be a non-empty array`);
		}
		const requirementIds = new Set<string>();
		const scenarioIds: string[] = [];
		for (const [requirementIndex, requirementValue] of journey.requirements.entries()) {
			const requirementLabel = `${label}.requirements[${requirementIndex}]`;
			const requirement = record(requirementValue, requirementLabel);
			exactKeys(requirement, ["id", "given", "when", "then"], requirementLabel);
			const requirementId = nonEmptyString(requirement.id, `${requirementLabel}.id`);
			if (!boundedId.test(requirementId) || requirementIds.has(requirementId)) {
				throw new Error(`${requirementLabel}.id must be unique and bounded`);
			}
			requirementIds.add(requirementId);
			requirementCount += 1;
			for (const field of ["given", "when", "then"] as const) {
				nonEmptyString(requirement[field], `${requirementLabel}.${field}`);
			}
			for (const client of clients) {
				for (const topology of topologies) {
					scenarioIds.push(`${journeyId}/${requirementId}/${client}/${topology}`);
				}
			}
		}
		if (new Set(scenarioIds).size !== scenarioIds.length) {
			throw new Error(`${label} generated duplicate scenario ids`);
		}

		const evidence = record(journey.evidence, `${label}.evidence`);
		exactKeys(evidence, ["defaultStatus", "requiredKinds", "records"], `${label}.evidence`);
		const requiredKinds = uniqueStrings(evidence.requiredKinds, `${label}.evidence.requiredKinds`);
		exactSet(requiredKinds, expected.requiredKinds, `${label}.evidence.requiredKinds`);
		if (!Array.isArray(evidence.records)) throw new Error(`${label}.evidence.records must be an array`);
		if (evidence.records.length > 0 && !context) {
			throw new Error(`${label}.evidence.records require repository and release validation context`);
		}
		const records = new Map<string, "partial" | "complete">();
		for (const [recordIndex, recordValue] of evidence.records.entries()) {
			if (!context) throw new Error(`${label}.evidence.records require validation context`);
			const recordLabel = `${label}.evidence.records[${recordIndex}]`;
			const evidenceRecord = record(recordValue, recordLabel);
			const scenarioId = nonEmptyString(evidenceRecord.scenarioId, `${recordLabel}.scenarioId`);
			const kind = nonEmptyString(evidenceRecord.kind, `${recordLabel}.kind`);
			exactKeys(
				evidenceRecord,
				kind === "performance"
					? ["scenarioId", "kind", "status", "reference", "environment"]
					: ["scenarioId", "kind", "status", "reference"],
				recordLabel,
			);
			if (!scenarioIds.includes(scenarioId) || !requiredKinds.includes(kind)) {
				throw new Error(`${recordLabel} must bind one generated scenario and required kind`);
			}
			if (evidenceRecord.status !== "partial" && evidenceRecord.status !== "complete") {
				throw new Error(`${recordLabel}.status must be partial or complete`);
			}
			const [, , client, topology] = scenarioId.split("/");
			const binding: JourneyScenarioBinding = { scenarioId, client, topology, kind };
			await validateJourneyEvidenceReference(
				evidenceRecord.reference,
				{
					repositoryRoot: context.repositoryRoot,
					release: {
						contractCommit: context.release.contractCommit,
						serverCommit: context.release.serverCommit,
						webCommit: context.release.webCommit,
						clientCommit: context.release.clientCommits[
							client as (typeof expectedClients)[number]
						],
					} satisfies JourneyEvidenceRelease,
					sourceAtCommit: context.sourceAtCommit,
					gateId: String(expected.gate) as JourneyGateId,
					kind,
					status: evidenceRecord.status,
					binding,
				},
			);
			if (kind === "performance") {
				await validatePerformanceEnvironment(
					evidenceRecord.environment,
					binding,
					{
						repositoryRoot: context.repositoryRoot,
						contractCommit: context.release.contractCommit,
						expectedAppCommit: context.release.clientCommits[
							client as (typeof expectedClients)[number]
						],
						sourceAtCommit: context.sourceAtCommit,
					},
				);
			}
			const key = `${scenarioId}\0${kind}`;
			if (records.has(key)) throw new Error(`${recordLabel} duplicates a scenario evidence kind`);
			records.set(key, evidenceRecord.status);
		}
		const derivedStatus = deriveStatus(scenarioIds, requiredKinds, records);
		if (status(evidence.defaultStatus, `${label}.evidence.defaultStatus`) !== derivedStatus) {
			throw new Error(`${label}.evidence.defaultStatus must be derived from its full matrix`);
		}
		journeyStatuses.set(journeyId, derivedStatus);
		for (const scenarioId of scenarioIds) {
			const [, , client, topology] = scenarioId.split("/");
			matrix.push({
				journeyId,
				owner: expected.owner,
				gate: expected.gate,
				scenarioId,
				client,
				topology,
				evidence: requiredKinds.map((kind) => ({
					kind,
					status: records.get(`${scenarioId}\0${kind}`) ?? "missing",
				})),
			});
		}
	}

	exactSet([...seenJourneys], Object.keys(immutableJourneyContract), "journey ids");
	if (matrix.length < 100) throw new Error("critical journey contract must generate at least 100 scenarios");
	matrix.sort((left, right) => compareText(left.scenarioId, right.scenarioId));
	const gateReadiness = Object.fromEntries(gateIds.map((gate) => {
		const ready = Object.entries(immutableJourneyContract)
			.filter(([, journey]) => String(journey.gate) === gate)
			.every(([id]) => journeyStatuses.get(id as JourneyId) === "complete");
		return [gate, ready ? "ready" : "not-ready"];
	})) as CriticalJourneySummary["gateReadiness"];
	for (const gate of gateIds) {
		if (declaredGateReadiness[gate] !== gateReadiness[gate]) {
			throw new Error(`gate ${gate} readiness must be derived from journey evidence`);
		}
	}
	const journeyStatus = [...journeyStatuses.values()].every((value) => value === "complete")
		? "ready"
		: "not-ready";
	if (document.journeyStatus !== journeyStatus) {
		throw new Error("critical journey readiness must be derived from journey evidence");
	}
	return {
		journeyCount: journeys.length,
		requirementCount,
		scenarioCount: matrix.length,
		journeyStatus,
		gateReadiness,
		matrixSha256: createHash("sha256").update(JSON.stringify(matrix)).digest("hex"),
	};
}
