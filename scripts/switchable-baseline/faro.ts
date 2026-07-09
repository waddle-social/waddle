import { createHash } from "node:crypto";
import { MAX_FARO_MEASUREMENT_VALUE } from "../../chat/src/lib/telemetry/measurement-contract";
import { parseJsonDocument } from "./json";
import {
	type BaselineSignal,
	compareText,
	type FaroQueryPlan,
	isRecord,
	type RequiredActivity,
} from "./model";

export interface FaroCollectionContext {
	webCommit: string;
	deploymentEnvironment: string;
	cluster: string;
	namespace: string;
	window: { start: string; end: string };
}

export interface NormalizedFaroAggregate {
	source: {
		sourceId: string;
		query: FaroQueryPlan;
		rawSha256: string;
		rowCount: number;
	};
	dimensions: Record<string, string[]>;
	series: Record<string, unknown>[];
}

function fail(message: string): never {
	throw new Error(`Faro aggregate: ${message}`);
}

function requireExactKeys(
	value: Record<string, unknown>,
	expected: readonly string[],
	label: string,
): void {
	const actual = Object.keys(value).sort(compareText);
	const sortedExpected = [...expected].sort(compareText);
	if (JSON.stringify(actual) !== JSON.stringify(sortedExpected)) {
		fail(`${label} must contain exactly ${sortedExpected.join(", ")}`);
	}
}

function requireCount(value: unknown, label: string): number {
	if (!Number.isSafeInteger(value) || (value as number) < 0) {
		fail(`${label} must be a non-negative safe integer`);
	}
	return value as number;
}

function requireFinite(value: unknown, label: string): number {
	if (
		typeof value !== "number"
		|| !Number.isFinite(value)
		|| value < 0
		|| value > MAX_FARO_MEASUREMENT_VALUE
	) {
		fail(
			`${label} must be a non-negative finite number no greater than ${MAX_FARO_MEASUREMENT_VALUE}`,
		);
	}
	return value;
}

function validatePercentiles(
	value: unknown,
	label: string,
	count: number,
): void {
	if (!isRecord(value)) fail(`${label} must be an object`);
	requireExactKeys(value, ["p50", "p95"], label);
	if (count === 0) {
		if (value.p50 !== null || value.p95 !== null) {
			fail(`${label} must use null percentiles when count is zero`);
		}
		return;
	}
	const p50 = requireFinite(value.p50, `${label}.p50`);
	const p95 = requireFinite(value.p95, `${label}.p95`);
	if (p95 < p50) fail(`${label}.p95 must be greater than or equal to p50`);
}

function canonicalAttributes(attributes: Record<string, string>): string {
	return JSON.stringify(Object.fromEntries(Object.entries(attributes).sort()));
}

function validateAttributes(
	row: Record<string, unknown>,
	signal: BaselineSignal,
	label: string,
): Record<string, string> {
	if (!isRecord(row.attributes)) fail(`${label}.attributes must be an object`);
	requireExactKeys(row.attributes, Object.keys(signal.attributes), `${label}.attributes`);
	const parsed: Record<string, string> = {};
	for (const [key, allowed] of Object.entries(signal.attributes)) {
		const value = row.attributes[key];
		if (typeof value !== "string" || !allowed.includes(value)) {
			fail(`${label}.attributes.${key} is outside the catalog closed set`);
		}
		parsed[key] = value;
	}
	return parsed;
}

function expectedAttributeCombinations(
	attributes: Record<string, string[]>,
): Record<string, string>[] {
	let combinations: Record<string, string>[] = [{}];
	for (const key of Object.keys(attributes).sort(compareText)) {
		combinations = combinations.flatMap((existing) =>
			[...attributes[key]].sort(compareText).map((value) => ({ ...existing, [key]: value })),
		);
	}
	return combinations;
}

function assertRequiredActivity(
	activity: Map<string, number>,
	required: RequiredActivity[] | undefined,
): void {
	for (const criterion of required ?? []) {
		const value = activity.get(canonicalAttributes(criterion.attributes));
		if (value === undefined || value < criterion.minimumValue) {
			fail(
				`required activity ${canonicalAttributes(criterion.attributes)} must be at least ${criterion.minimumValue}`,
			);
		}
	}
}

export function validateFaroSeries(
	signal: BaselineSignal,
	value: unknown,
): Record<string, unknown>[] {
	if (signal.source !== "faro" || signal.collection !== "manual-export") {
		fail(`${signal.id} is not a Faro manual-export signal`);
	}
	if (!Array.isArray(value) || value.length === 0) {
		fail(`${signal.id}.series must be a non-empty array`);
	}
	const activity = new Map<string, number>();
	const rows = value.map((entry, index) => {
		if (!isRecord(entry)) fail(`${signal.id}.series[${index}] must be an object`);
		const label = `${signal.id}.series[${index}]`;
		const attributes = validateAttributes(entry, signal, label);
		let count: number;
		switch (signal.id) {
			case "browser-auth-bootstrap": {
				requireExactKeys(entry, ["attributes", "count", "durationMs"], label);
				count = requireCount(entry.count, `${label}.count`);
				if (!isRecord(entry.durationMs)) fail(`${label}.durationMs must be an object`);
				requireExactKeys(entry.durationMs, ["count", "p50", "p95"], `${label}.durationMs`);
				const durationCount = requireCount(entry.durationMs.count, `${label}.durationMs.count`);
				if (durationCount !== count) fail(`${label} event and duration counts must match`);
				validatePercentiles(
					{ p50: entry.durationMs.p50, p95: entry.durationMs.p95 },
					`${label}.durationMs`,
					count,
				);
				break;
			}
			case "browser-message-ack-latency":
				requireExactKeys(entry, ["attributes", "count", "latencyMs"], label);
				count = requireCount(entry.count, `${label}.count`);
				validatePercentiles(entry.latencyMs, `${label}.latencyMs`, count);
				break;
			case "browser-session-lifecycle":
				requireExactKeys(entry, ["attributes", "count"], label);
				count = requireCount(entry.count, `${label}.count`);
				break;
			case "browser-reconnect-duration":
				requireExactKeys(entry, ["attributes", "count", "durationMs"], label);
				count = requireCount(entry.count, `${label}.count`);
				validatePercentiles(entry.durationMs, `${label}.durationMs`, count);
				break;
			default:
				fail(`unsupported Faro signal ${signal.id}`);
		}
		activity.set(canonicalAttributes(attributes), count);
		return entry;
	});

	const actual = rows.map((row) =>
		canonicalAttributes(row.attributes as Record<string, string>)
	);
	const expected = expectedAttributeCombinations(signal.attributes).map(canonicalAttributes);
	if (JSON.stringify(actual) !== JSON.stringify(expected)) {
		fail(`${signal.id}.series must contain the complete sorted closed attribute set`);
	}
	assertRequiredActivity(activity, signal.requiredActivity);
	return rows;
}

export function materializeFaroQueryPlan(
	signal: BaselineSignal,
	context: FaroCollectionContext,
): FaroQueryPlan {
	if (!signal.faroQuery) fail(`${signal.id} has no typed Faro query definition`);
	return {
		schemaVersion: 1,
		engine: "grafana-faro-aggregate",
		sourceId: signal.faroQuery.sourceId,
		deploymentEnvironment: context.deploymentEnvironment,
		cluster: context.cluster,
		namespace: context.namespace,
		release: context.webCommit,
		window: { ...context.window },
		signalNames: [...signal.faroQuery.signalNames],
		groupBy: [...signal.faroQuery.groupBy],
		aggregates: [...signal.faroQuery.aggregates],
	};
}

export function normalizeFaroAggregateExport(
	rawExport: string,
	signal: BaselineSignal,
	context: FaroCollectionContext,
): NormalizedFaroAggregate {
	const parsed = parseJsonDocument(rawExport, "restricted Faro aggregate export");
	if (!isRecord(parsed)) fail("restricted export must be an object");
	requireExactKeys(parsed, ["schemaVersion", "query", "source", "rows"], "restricted export");
	if (parsed.schemaVersion !== 1) fail("restricted export schemaVersion must be 1");
	const query = materializeFaroQueryPlan(signal, context);
	if (JSON.stringify(parsed.query) !== JSON.stringify(query)) {
		fail("restricted export query must match the exact release/deployment/window plan");
	}
	if (!isRecord(parsed.source)) fail("restricted export source must be an object");
	requireExactKeys(parsed.source, ["sourceId"], "restricted export source");
	if (
		typeof parsed.source.sourceId !== "string"
		|| !/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(parsed.source.sourceId)
		|| parsed.source.sourceId !== query.sourceId
	) {
		fail("restricted export source.sourceId must be the bounded catalog source id");
	}
	const series = validateFaroSeries(signal, parsed.rows);
	return {
		source: {
			sourceId: parsed.source.sourceId,
			query,
			rawSha256: createHash("sha256").update(rawExport).digest("hex"),
			rowCount: series.length,
		},
		dimensions: Object.fromEntries(
			Object.entries(signal.attributes).map(([key, values]) => [
				key,
				[...values].sort(compareText),
			]),
		),
		series,
	};
}
