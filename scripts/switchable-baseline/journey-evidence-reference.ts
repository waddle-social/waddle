import { extname } from "node:path";
import {
	parseWindow,
	requireExactKeys,
	requireRecord,
	requireSha256,
	requireString,
	requireUtcInstant,
} from "./gate-evidence/common";
import {
	readPinnedFile,
	readTrustedJsonSnapshot,
	requireRepositorySourceAtCommit,
	resolveTrustedRepositoryFile,
	type RepositorySourceAtCommitReader,
} from "./gate-evidence/filesystem";

export type JourneyGateId = "2" | "3" | "4";

export interface JourneyScenarioBinding {
	scenarioId: string;
	client: string;
	topology: string;
	kind: string;
}

export interface JourneyEvidenceReferenceContext {
	repositoryRoot: string;
	release: JourneyEvidenceRelease;
	sourceAtCommit?: RepositorySourceAtCommitReader;
	gateId: JourneyGateId;
	kind: string;
	status: "partial" | "complete";
	binding: JourneyScenarioBinding;
}

export interface JourneyEvidenceRelease {
	contractCommit: string;
	serverCommit: string;
	webCommit: string;
	clientCommit: string;
}

const repositoryTestPath = new RegExp(
	"^(?:chat/tests/|server/crates/(?:[^/]+/tests/|waddle-server/src/server/routes/websocket/tests/)"
		+ "|apps/apple/.+/Tests/|tests/)",
);
const testIdPattern = /^[A-Za-z][A-Za-z0-9_. :>-]+$/;
const ciRunUrl = /^https:\/\/github\.com\/waddle-social\/waddle\/actions\/runs\/[1-9][0-9]*$/;

function requireCommit(value: unknown, label: string): string {
	if (typeof value !== "string" || !/^[0-9a-f]{40}$/.test(value)) {
		throw new Error(`${label} must be a full lowercase Git SHA`);
	}
	return value;
}

function requireExactBinding(
	reference: Record<string, unknown>,
	binding: JourneyScenarioBinding,
): void {
	for (const [key, expected] of Object.entries(binding)) {
		if (reference[key] !== expected) {
			throw new Error(`journey evidence reference.${key} must match its exact scenario scope`);
		}
	}
}

export function scenarioTestId(binding: JourneyScenarioBinding): string {
	return ["switchable", ...binding.scenarioId.split("/"), binding.kind]
		.join("__")
		.replaceAll("-", "_");
}

function sourceDefinesTest(source: string, path: string, testId: string): boolean {
	const escaped = testId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
	switch (extname(path)) {
		case ".rs":
			return new RegExp(
				`#\\[(?:tokio::)?test\\][\\s\\S]{0,240}(?:async\\s+)?fn\\s+${escaped}\\s*\\(`,
			).test(source);
		case ".ts":
			return new RegExp(`(?:test|it)\\(\\s*["']${escaped}["']`).test(source);
		case ".swift": {
			const swiftTesting = new RegExp(`@Test[\\s\\S]{0,240}func\\s+${escaped}\\s*\\(`)
				.test(source);
			const xctest = /^test[A-Z0-9_]/.test(testId)
				&& new RegExp(`func\\s+${escaped}\\s*\\(`).test(source);
			return swiftTesting || xctest;
		}
		case ".cue":
			return new RegExp(`name:\\s*["']${escaped}["']`).test(source);
		default:
			return false;
	}
}

async function validateRepositoryTest(
	reference: Record<string, unknown>,
	context: JourneyEvidenceReferenceContext,
): Promise<void> {
	requireExactKeys(reference, [
		"type", "path", "testId", "scenarioId", "client", "topology", "kind",
	], "journey repo-test reference");
	requireExactBinding(reference, context.binding);
	const path = requireString(reference, "path", "journey repo-test reference");
	if (!repositoryTestPath.test(path)) {
		throw new Error("journey repo-test path is outside the supported test roots");
	}
	const testId = requireString(reference, "testId", "journey repo-test reference");
	if (!testIdPattern.test(testId) || testId !== scenarioTestId(context.binding)) {
		throw new Error("journey repo-test must use its exact deterministic scenario test id");
	}
	const sourcePath = resolveTrustedRepositoryFile(
		context.repositoryRoot,
		path,
		path.split("/", 1)[0],
		"journey repo-test source",
	);
	const source = readPinnedFile(sourcePath, "journey repo-test source");
	if (!sourceDefinesTest(source.bytes.toString("utf8"), path, testId)) {
		throw new Error("journey repo-test source does not define its exact test id");
	}
	await requireRepositorySourceAtCommit(
		context.repositoryRoot,
		context.release.contractCommit,
		path,
		"journey repo-test source",
		context.sourceAtCommit,
		source,
	);
}

function validateCiRun(
	reference: Record<string, unknown>,
	context: JourneyEvidenceReferenceContext,
): void {
	requireExactKeys(reference, ["type", "url", "commit"], "journey ci-run reference");
	const url = requireString(reference, "url", "journey ci-run reference");
	if (!ciRunUrl.test(url)) throw new Error("journey ci-run must use a canonical Waddle Actions URL");
	if (requireCommit(reference.commit, "journey ci-run commit") !== context.release.contractCommit) {
		throw new Error("journey ci-run commit must match the journey-baseline release commit");
	}
}

function validateManualReport(
	reference: Record<string, unknown>,
	context: JourneyEvidenceReferenceContext,
): void {
	requireExactKeys(reference, ["type", "path", "sha256"], "journey manual-report reference");
	const path = requireString(reference, "path", "journey manual-report reference");
	const expectedSha256 = requireSha256(
		reference.sha256,
		"journey manual-report reference.sha256",
	);
	const reportPath = resolveTrustedRepositoryFile(
		context.repositoryRoot,
		path,
		"docs/evidence",
		"journey manual-report reference.path",
	);
	if (!path.endsWith(".md")) throw new Error("journey manual-report must name a Markdown file");
	if (readPinnedFile(reportPath, "journey manual-report").sha256 !== expectedSha256) {
		throw new Error("journey manual-report digest does not match its bytes");
	}
}

function validateManualSchema(
	reference: Record<string, unknown>,
	context: JourneyEvidenceReferenceContext,
): void {
	requireExactKeys(reference, [
		"type", "path", "sha256", "schema", "scenarioId", "client", "topology", "kind",
	], "journey manual-schema reference");
	requireExactBinding(reference, context.binding);
	const expectedSchema = `switchable-evidence/gate-${context.gateId}/${context.kind}/v1`;
	const expectedPath = `docs/evidence/gate-${context.gateId}/journeys/`
		+ `${context.binding.scenarioId}/${context.kind}.json`;
	if (reference.path !== expectedPath || reference.schema !== expectedSchema) {
		throw new Error("journey manual-schema reference must use its canonical path and schema");
	}
	const snapshot = readTrustedJsonSnapshot(
		context.repositoryRoot,
		expectedPath,
		"docs/evidence",
		"journey manual-schema artifact",
		".json",
		requireSha256(reference.sha256, "journey manual-schema reference.sha256"),
	);
	const report = requireRecord(snapshot.value, "journey manual-schema artifact");
	requireExactKeys(report, [
		"schemaVersion", "schema", "evidenceKind", "status", "scope", "release", "window",
		"capturedAt", "assertions",
	], "journey manual-schema artifact");
	if (
		report.schemaVersion !== 1
		|| report.schema !== expectedSchema
		|| report.evidenceKind !== context.kind
		|| report.status !== "complete"
	) throw new Error("journey manual-schema artifact does not match its typed evidence contract");
	const scope = requireRecord(report.scope, "journey manual-schema artifact.scope");
	requireExactKeys(scope, ["type", "scenarioId", "client", "topology"], "journey manual-schema artifact.scope");
	if (
		scope.type !== "scenario"
		|| scope.scenarioId !== context.binding.scenarioId
		|| scope.client !== context.binding.client
		|| scope.topology !== context.binding.topology
	) throw new Error("journey manual-schema artifact scope does not match its scenario");
	const release = requireRecord(report.release, "journey manual-schema artifact.release");
	requireExactKeys(
		release,
		["serverCommit", "webCommit", "appCommit"],
		"journey manual-schema artifact.release",
	);
	if (
		requireCommit(release.serverCommit, "journey manual-schema server commit")
			!== context.release.serverCommit
		|| requireCommit(release.webCommit, "journey manual-schema web commit")
			!== context.release.webCommit
		|| requireCommit(release.appCommit, "journey manual-schema app commit")
			!== context.release.clientCommit
	) throw new Error("journey manual-schema artifact release does not match the immutable journey release");
	const window = parseWindow(report.window, "journey manual-schema artifact.window");
	const capturedAt = requireUtcInstant(
		report.capturedAt,
		"journey manual-schema artifact.capturedAt",
	);
	if (Date.parse(capturedAt) < Date.parse(window.end)) {
		throw new Error("journey manual-schema artifact capture must follow its evidence window");
	}
	if (!Array.isArray(report.assertions) || report.assertions.length === 0) {
		throw new Error("journey manual-schema artifact must contain passing assertions");
	}
	const assertionIds = new Set<string>();
	for (const [index, value] of report.assertions.entries()) {
		const assertion = requireRecord(value, `journey manual-schema artifact.assertions[${index}]`);
		requireExactKeys(assertion, ["id", "status", "observed"], `journey manual-schema artifact.assertions[${index}]`);
		const id = requireString(assertion, "id", `journey manual-schema artifact.assertions[${index}]`);
		if (!/^[a-z][a-z0-9-]+$/.test(id) || assertionIds.has(id) || assertion.status !== "pass") {
			throw new Error("journey manual-schema assertions must be unique bounded passes");
		}
		assertionIds.add(id);
		if (
			typeof assertion.observed !== "boolean"
			&& (typeof assertion.observed !== "number" || !Number.isFinite(assertion.observed))
		) throw new Error("journey manual-schema observed values must be privacy-safe scalars");
	}
}

function requireCompleteReferencePolicy(
	context: JourneyEvidenceReferenceContext,
): void {
	if (context.status !== "complete") return;
	throw new Error(
		"journey evidence cannot complete without a verified passing-run or manual/live evidence attestation with a kind-specific contract",
	);
}

export async function validateJourneyEvidenceReference(
	value: unknown,
	context: JourneyEvidenceReferenceContext,
): Promise<void> {
	const reference = requireRecord(value, "journey evidence reference");
	const type = requireString(reference, "type", "journey evidence reference");
	switch (type) {
		case "repo-test":
			await validateRepositoryTest(reference, context);
			break;
		case "ci-run":
			validateCiRun(reference, context);
			break;
		case "manual-report":
			validateManualReport(reference, context);
			break;
		case "manual-schema":
			validateManualSchema(reference, context);
			break;
		default:
			throw new Error(`unsupported journey evidence reference type ${type}`);
	}
	requireCompleteReferencePolicy(context);
}
