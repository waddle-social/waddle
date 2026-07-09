import {
  type BaselineSignal,
  type DeploymentScope,
  IDENTITY_ATTRIBUTE_BINDINGS,
  type PrivacyContract,
  SCOPE_QUERY_PLACEHOLDER,
  type QueryPlaceholder,
} from "../model";
import { normalizePrivacyName } from "./parsing";

function assertPrivacySafeName(
	name: string,
	privacy: PrivacyContract,
	context: string,
): void {
	const normalizedName = normalizePrivacyName(name);
	for (const fragment of privacy.forbiddenAttributeFragments) {
		if (normalizedName.includes(normalizePrivacyName(fragment))) {
			throw new Error(`${context} contains forbidden privacy fragment ${fragment}`);
		}
	}
}
function assertSignalPrivacy(
	signal: BaselineSignal,
	privacy: PrivacyContract,
): void {
	for (const [attribute, values] of Object.entries(signal.attributes)) {
		assertPrivacySafeName(
			attribute,
			privacy,
			`catalog signal ${signal.id} attribute ${attribute}`,
		);
		if (values.length > privacy.maximumValuesPerAttribute) {
			throw new Error(
				`catalog signal ${signal.id} attribute ${attribute} exceeds maximumValuesPerAttribute`,
			);
		}
	}

	for (const selector of signal.query.matchAll(/\{([^}]*)\}/g)) {
		for (const label of selector[1].matchAll(
			/([a-zA-Z_:][a-zA-Z0-9_:]*)\s*(?:=|!=|=~|!~)/g,
		)) {
			assertPrivacySafeName(
				label[1],
				privacy,
				`catalog signal ${signal.id} query selector label ${label[1]}`,
			);
		}
	}
}

function escapeRegex(value: string): string {
	return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function assertQueryScope(signal: BaselineSignal): void {
	const declared = new Set([SCOPE_QUERY_PLACEHOLDER]);
	for (const match of signal.query.matchAll(/{{[^{}]+}}/g)) {
		if (!declared.has(match[0] as QueryPlaceholder)) {
			throw new Error(
				`automated Prometheus signal ${signal.id} uses unknown placeholder ${match[0]}`,
			);
		}
	}

	const withoutScopeTokens = signal.query.replaceAll(
		SCOPE_QUERY_PLACEHOLDER,
		"",
	);
	if (/\{[^{}]*\}/.test(withoutScopeTokens)) {
		throw new Error(
			`automated Prometheus signal ${signal.id} must not define ad-hoc label selectors`,
		);
	}

	let metricSelectorCount = 0;
	for (const metricName of signal.metricNames) {
		const metricPattern = new RegExp(
			`(?:^|[^a-zA-Z0-9_:])${escapeRegex(metricName)}(?:$|[^a-zA-Z0-9_:])`,
			"g",
		);
		const scopedMetricPattern = new RegExp(
			`(?:^|[^a-zA-Z0-9_:])${escapeRegex(metricName)}${escapeRegex(SCOPE_QUERY_PLACEHOLDER)}`,
			"g",
		);
		const occurrences = [...signal.query.matchAll(metricPattern)].length;
		const scopedOccurrences = [
			...signal.query.matchAll(scopedMetricPattern),
		].length;
		if (occurrences === 0 || occurrences !== scopedOccurrences) {
			throw new Error(
				`automated Prometheus signal ${signal.id} must apply ${SCOPE_QUERY_PLACEHOLDER} directly to every ${metricName} selector`,
			);
		}
		metricSelectorCount += occurrences;
	}
	if (
		[...signal.query.matchAll(/{{scope}}/g)].length !== metricSelectorCount
	) {
		throw new Error(
			`automated Prometheus signal ${signal.id} contains an unbound scope selector`,
		);
	}
}

function rangeLookbackSeconds(signal: BaselineSignal): number {
	let maximum = 0;
	for (const match of signal.query.matchAll(/\[([^\]]+)\]/g)) {
		const duration = match[1];
		let consumed = 0;
		let seconds = 0;
		for (const part of duration.matchAll(/([1-9][0-9]*)([smhdw])/g)) {
			if (part.index !== consumed) {
				throw new Error(
					`automated Prometheus signal ${signal.id} has an unsupported range duration`,
				);
			}
			const multiplier = {
				s: 1,
				m: 60,
				h: 60 * 60,
				d: 24 * 60 * 60,
				w: 7 * 24 * 60 * 60,
			}[part[2] as "s" | "m" | "h" | "d" | "w"];
			seconds += Number(part[1]) * multiplier;
			consumed += part[0].length;
		}
		if (consumed !== duration.length) {
			throw new Error(
				`automated Prometheus signal ${signal.id} has an unsupported range duration`,
			);
		}
		maximum = Math.max(maximum, seconds);
	}
	return maximum;
}


export function validateCatalogSemantics(
  deploymentScope: DeploymentScope,
  privacy: PrivacyContract,
  signals: BaselineSignal[],
): void {
	const ids = signals.map(({ id }) => id);
	if (new Set(ids).size !== ids.length) {
		throw new Error("baseline signal catalog contains duplicate signal ids");
	}

	const automatedSignals = signals.filter(
		({ source, collection }) =>
			source === "prometheus" && collection === "automated",
	);
	if (automatedSignals.length === 0) {
		throw new Error("catalog defines no automated Prometheus signals");
	}
	for (const signal of signals) assertSignalPrivacy(signal, privacy);
	for (const signal of automatedSignals) {
		assertQueryScope(signal);
	}
	const maximumQueryLookbackSeconds = Math.max(
		...automatedSignals.map(rangeLookbackSeconds),
	);
	if (
		deploymentScope.maximumRangeLookbackSeconds < maximumQueryLookbackSeconds
	) {
		throw new Error(
			"catalog deployment identity lookback does not cover the maximum Prometheus range lookback",
		);
	}
	for (const signal of automatedSignals) {
		if (
			signal.collectionLookbackSeconds !== undefined
			&& signal.collectionLookbackSeconds
				> deploymentScope.maximumRangeLookbackSeconds
		) {
			throw new Error(
				`automated Prometheus signal ${signal.id} collection lookback exceeds the deployment maximum`,
			);
		}
	}

	const targetSignals = automatedSignals.filter(
		({ id }) => id === deploymentScope.targetSignalId,
	);
	if (targetSignals.length !== 1) {
		throw new Error(
			"catalog deployment scope must identify one automated target signal",
		);
	}
	const targetSignal = targetSignals[0];
	if (
		targetSignal.metricNames.length !== 1 ||
		targetSignal.metricNames[0] !== deploymentScope.identityMetric
	) {
		throw new Error(
			"deployment target signal metricNames must equal exactly identityMetric",
		);
	}
	if (
		targetSignal.collectionLookbackSeconds
		!== deploymentScope.maximumRangeLookbackSeconds
	) {
		throw new Error(
			"deployment target signal collection lookback must equal the maximum range lookback",
		);
	}
	const expectedIdentityAttributes = Object.fromEntries(
		IDENTITY_ATTRIBUTE_BINDINGS.map(({ attribute, placeholder }) => [
			attribute,
			[placeholder],
		]),
	);
	if (
		JSON.stringify(targetSignal.attributes) !==
		JSON.stringify(expectedIdentityAttributes)
	) {
		throw new Error(
			"deployment target signal attributes must declare the exact build identity labels",
		);
	}
}
