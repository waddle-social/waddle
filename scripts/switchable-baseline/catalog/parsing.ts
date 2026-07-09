import {
  type BaselineSignal,
  compareText,
  DEPLOYMENT_SCOPE_BINDINGS,
  type DeploymentScope,
  type DeploymentScopeLabel,
  type FaroQueryDefinition,
  isRecord,
  type PrivacyContract,
  QUERY_STEP_SECONDS,
  SCOPE_QUERY_PLACEHOLDER,
  type QueryPlaceholder,
} from "../model";

export function requireExactKeys(
	record: Record<string, unknown>,
	expected: readonly string[],
	label: string,
): void {
	const actual = Object.keys(record).sort(compareText);
	const expectedSorted = [...expected].sort(compareText);
	if (JSON.stringify(actual) !== JSON.stringify(expectedSorted)) {
		throw new Error(`${label} must contain exactly ${expectedSorted.join(", ")}`);
	}
}

export function requireString(record: Record<string, unknown>, key: string): string {
	const value = record[key];
	if (typeof value !== "string" || value.trim().length === 0) {
		throw new Error(`catalog field ${key} must be a non-empty string`);
	}
	return value;
}

function requireStringArray(
	record: Record<string, unknown>,
	key: string,
): string[] {
	const value = record[key];
	if (
		!Array.isArray(value) ||
		value.length === 0 ||
		value.some((item) => typeof item !== "string" || item.length === 0)
	) {
		throw new Error(`catalog field ${key} must be a non-empty string array`);
	}
	return [...value] as string[];
}

function parseAttributes(value: unknown): Record<string, string[]> {
	if (!isRecord(value)) {
		throw new Error("catalog signal attributes must be an object");
	}

	const attributes: Record<string, string[]> = {};
	for (const key of Object.keys(value).sort()) {
		const values = value[key];
		if (
			!Array.isArray(values) ||
			values.length === 0 ||
			values.some((item) => typeof item !== "string" || item.length === 0)
		) {
			throw new Error(
				"catalog signal attribute values must be non-empty string arrays",
			);
		}
		const sortedValues = [...values].sort() as string[];
		if (new Set(sortedValues).size !== sortedValues.length) {
			throw new Error(`catalog signal attribute ${key} contains duplicates`);
		}
		attributes[key] = sortedValues;
	}
	return attributes;
}

function parseRequiredActivity(
	value: unknown,
	attributes: Record<string, string[]>,
): BaselineSignal["requiredActivity"] {
	if (value === undefined) return undefined;
	if (!Array.isArray(value) || value.length === 0) {
		throw new Error("catalog signal requiredActivity must be a non-empty array");
	}
	const expectedKeys = Object.keys(attributes).sort(compareText);
	const seen = new Set<string>();
	return value.map((entry, index) => {
		if (!isRecord(entry)) throw new Error("catalog requiredActivity entries must be objects");
		requireExactKeys(entry, ["attributes", "minimumValue"], `catalog requiredActivity[${index}]`);
		if (!isRecord(entry.attributes)) {
			throw new Error("catalog requiredActivity attributes must be an object");
		}
		requireExactKeys(entry.attributes, expectedKeys, `catalog requiredActivity[${index}].attributes`);
		const parsedAttributes: Record<string, string> = {};
		for (const key of expectedKeys) {
			const attribute = entry.attributes[key];
			if (typeof attribute !== "string" || !attributes[key].includes(attribute)) {
				throw new Error(`catalog requiredActivity attribute ${key} is outside the closed set`);
			}
			parsedAttributes[key] = attribute;
		}
		if (
			typeof entry.minimumValue !== "number"
			|| !Number.isFinite(entry.minimumValue)
			|| entry.minimumValue <= 0
		) throw new Error("catalog requiredActivity minimumValue must be positive and finite");
		const key = JSON.stringify(parsedAttributes);
		if (seen.has(key)) throw new Error("catalog requiredActivity contains duplicate selectors");
		seen.add(key);
		return { attributes: parsedAttributes, minimumValue: entry.minimumValue };
	});
}

function parseFaroQuery(
	value: unknown,
	metricNames: string[],
	attributes: Record<string, string[]>,
): FaroQueryDefinition {
	if (!isRecord(value)) throw new Error("catalog Faro signal must define faroQuery");
	requireExactKeys(value, ["sourceId", "signalNames", "groupBy", "aggregates"], "catalog faroQuery");
	if (value.sourceId !== "waddle-chat") {
		throw new Error("catalog faroQuery.sourceId must be waddle-chat");
	}
	const signalNames = requireStringArray(value, "signalNames").sort(compareText);
	if (JSON.stringify(signalNames) !== JSON.stringify([...metricNames].sort(compareText))) {
		throw new Error("catalog faroQuery.signalNames must match metricNames");
	}
	const groupByValue = value.groupBy;
	if (!Array.isArray(groupByValue) || groupByValue.some((entry) => typeof entry !== "string")) {
		throw new Error("catalog faroQuery.groupBy must be a string array");
	}
	const groupBy = [...groupByValue].sort(compareText) as string[];
	if (JSON.stringify(groupBy) !== JSON.stringify(Object.keys(attributes).sort(compareText))) {
		throw new Error("catalog faroQuery.groupBy must match the signal attributes");
	}
	const aggregates = requireStringArray(value, "aggregates").sort(compareText);
	if (aggregates.some((entry) => !/^[a-z][a-z0-9_]{0,63}$/.test(entry))) {
		throw new Error("catalog faroQuery.aggregates must use bounded names");
	}
	return { sourceId: "waddle-chat", signalNames, groupBy, aggregates };
}

export function parseSignal(value: unknown): BaselineSignal {
	if (!isRecord(value)) {
		throw new Error("catalog signals must be objects");
	}

	const minimumAllowedValue = value.minimumAllowedValue;
	const maximumAllowedValue = value.maximumAllowedValue;
	const collectionLookbackSeconds = value.collectionLookbackSeconds;
	const requiredStability = value.requiredStability;
	for (const [name, allowedValue] of [
		["minimumAllowedValue", minimumAllowedValue],
		["maximumAllowedValue", maximumAllowedValue],
	] as const) {
		if (
			allowedValue !== undefined &&
			(typeof allowedValue !== "number" || !Number.isFinite(allowedValue))
		) {
			throw new Error(`catalog signal ${name} must be a finite number`);
		}
	}
	if (
		typeof minimumAllowedValue === "number" &&
		typeof maximumAllowedValue === "number" &&
		minimumAllowedValue > maximumAllowedValue
	) {
		throw new Error(
			"catalog signal minimumAllowedValue must not exceed maximumAllowedValue",
		);
	}
	if (
		collectionLookbackSeconds !== undefined
		&& (
			!Number.isInteger(collectionLookbackSeconds)
			|| (collectionLookbackSeconds as number) <= 0
			|| (collectionLookbackSeconds as number) % QUERY_STEP_SECONDS !== 0
		)
	) {
		throw new Error(
			"catalog signal collectionLookbackSeconds must be a positive whole number of query steps",
		);
	}
	if (requiredStability !== undefined && requiredStability !== "constant") {
		throw new Error("catalog signal requiredStability must be constant");
	}

	const source = requireString(value, "source");
	const collection = requireString(value, "collection");
	const metricNames = requireStringArray(value, "metricNames").sort(compareText);
	const attributes = parseAttributes(value.attributes);
	const isFaro = source === "faro" && collection === "manual-export";
	const isPrometheus = source === "prometheus" && collection === "automated";
	if (!isFaro && !isPrometheus) {
		throw new Error("catalog signals must be automated Prometheus or manual-export Faro signals");
	}
	const expectedKeys = [
		"id", "owner", "source", "collection", "kind", "metricNames", "attributes",
		"unit", "query", "window", "interpretation", "limitations",
	];
	if (minimumAllowedValue !== undefined) expectedKeys.push("minimumAllowedValue");
	if (maximumAllowedValue !== undefined) expectedKeys.push("maximumAllowedValue");
	if (collectionLookbackSeconds !== undefined) expectedKeys.push("collectionLookbackSeconds");
	if (requiredStability !== undefined) expectedKeys.push("requiredStability");
	if (value.requiredActivity !== undefined) expectedKeys.push("requiredActivity");
	if (isFaro) expectedKeys.push("faroQuery");
	requireExactKeys(value, expectedKeys, "catalog signal");
	if (isFaro && (collectionLookbackSeconds !== undefined || requiredStability !== undefined)) {
		throw new Error(
			"catalog Faro signals must not define Prometheus collection continuity fields",
		);
	}

	return {
		id: requireString(value, "id"),
		owner: requireString(value, "owner"),
		source,
		collection,
		kind: requireString(value, "kind"),
		metricNames,
		attributes,
		unit: requireString(value, "unit"),
		query: requireString(value, "query"),
		window: requireString(value, "window"),
		...(collectionLookbackSeconds === undefined
			? {}
			: { collectionLookbackSeconds: collectionLookbackSeconds as number }),
		...(requiredStability === undefined
			? {}
			: { requiredStability: "constant" as const }),
		...(minimumAllowedValue === undefined ? {} : { minimumAllowedValue }),
		...(maximumAllowedValue === undefined ? {} : { maximumAllowedValue }),
		interpretation: requireString(value, "interpretation"),
		limitations: requireString(value, "limitations"),
		...(value.requiredActivity === undefined
			? {}
			: { requiredActivity: parseRequiredActivity(value.requiredActivity, attributes) }),
		...(isFaro ? { faroQuery: parseFaroQuery(value.faroQuery, metricNames, attributes) } : {}),
	};
}
function sameStringSet(actual: string[], expected: readonly string[]): boolean {
	return (
		JSON.stringify([...actual].sort(compareText)) ===
		JSON.stringify([...expected].sort(compareText))
	);
}

export function parseDeploymentScope(value: unknown): DeploymentScope {
	if (!isRecord(value)) {
		throw new Error("catalog deploymentScope must be an object");
	}
	requireExactKeys(value, [
		"identityMetric",
		"targetSignalId",
		"maximumRangeLookbackSeconds",
		"requiredLabels",
		"queryPlaceholders",
	], "catalog deploymentScope");

	const requiredLabels = requireStringArray(value, "requiredLabels");
	const queryPlaceholders = requireStringArray(value, "queryPlaceholders");
	const expectedLabels = DEPLOYMENT_SCOPE_BINDINGS.map(({ label }) => label);
	const expectedPlaceholders = [SCOPE_QUERY_PLACEHOLDER];
	if (!sameStringSet(requiredLabels, expectedLabels)) {
		throw new Error(
			`catalog deploymentScope.requiredLabels must be exactly ${expectedLabels.join(", ")}`,
		);
	}
	if (!sameStringSet(queryPlaceholders, expectedPlaceholders)) {
		throw new Error(
			"catalog deploymentScope.queryPlaceholders must declare every supported scope placeholder",
		);
	}

	const identityMetric = requireString(value, "identityMetric");
	if (!/^[a-zA-Z_:][a-zA-Z0-9_:]*$/.test(identityMetric)) {
		throw new Error("catalog deploymentScope.identityMetric is not a metric name");
	}

	const maximumRangeLookbackSeconds = value.maximumRangeLookbackSeconds;
	if (
		!Number.isInteger(maximumRangeLookbackSeconds) ||
		(maximumRangeLookbackSeconds as number) <= 0 ||
		(maximumRangeLookbackSeconds as number) % QUERY_STEP_SECONDS !== 0
	) {
		throw new Error(
			"catalog deploymentScope.maximumRangeLookbackSeconds must be a positive whole number of query steps",
		);
	}

	return {
		identityMetric,
		targetSignalId: requireString(value, "targetSignalId"),
		maximumRangeLookbackSeconds: maximumRangeLookbackSeconds as number,
		requiredLabels: requiredLabels as DeploymentScopeLabel[],
		queryPlaceholders: queryPlaceholders as QueryPlaceholder[],
	};
}

export function normalizePrivacyName(value: string): string {
	return value.replace(/[-_.]/g, "").toLowerCase();
}

export function parsePrivacyContract(value: unknown): PrivacyContract {
	if (!isRecord(value)) {
		throw new Error("catalog privacy must be an object");
	}
	requireExactKeys(value, [
		"forbiddenAttributeFragments",
		"maximumValuesPerAttribute",
		"prohibitedPayloads",
	], "catalog privacy");
	const forbiddenAttributeFragments = requireStringArray(
		value,
		"forbiddenAttributeFragments",
	).map((fragment) => fragment.trim());
	const normalizedFragments = forbiddenAttributeFragments.map(
		normalizePrivacyName,
	);
	if (
		normalizedFragments.some((fragment) => fragment.length === 0) ||
		new Set(normalizedFragments).size !== normalizedFragments.length
	) {
		throw new Error(
			"catalog privacy forbidden attribute fragments must be unique and non-empty after normalization",
		);
	}

	const maximumValuesPerAttribute = value.maximumValuesPerAttribute;
	if (
		!Number.isInteger(maximumValuesPerAttribute) ||
		(maximumValuesPerAttribute as number) <= 0 ||
		(maximumValuesPerAttribute as number) > 32
	) {
		throw new Error(
			"catalog privacy maximumValuesPerAttribute must be an integer from 1 to 32",
		);
	}

	const prohibitedPayloads = requireStringArray(value, "prohibitedPayloads").map(
		(description) => description.trim(),
	);
	if (prohibitedPayloads.some((description) => description.length === 0)) {
		throw new Error(
			"catalog privacy prohibited payload descriptions must be non-empty",
		);
	}

	return {
		forbiddenAttributeFragments,
		maximumValuesPerAttribute: maximumValuesPerAttribute as number,
		prohibitedPayloads,
	};
}
