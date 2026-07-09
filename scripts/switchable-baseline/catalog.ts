import { type BaselineCatalog, isRecord } from "./model";
import {
  parseDeploymentScope,
  parsePrivacyContract,
  parseSignal,
  requireExactKeys,
  requireString,
} from "./catalog/parsing";
import { validateCatalogSemantics } from "./catalog/validation";

export {
  assertGateZeroSignalSet,
  materializePrometheusSignal,
  materializePrometheusSignals,
  selectAutomatedPrometheusSignals,
} from "./catalog/materialize";

export function parseBaselineCatalog(value: unknown): BaselineCatalog {
	if (!isRecord(value)) {
		throw new Error("baseline signal catalog must be an object");
	}
	requireExactKeys(value, [
		"schemaVersion",
		"milestone",
		"minimumCollectionWindowMinutes",
		"deploymentScope",
		"privacy",
		"signals",
	], "baseline signal catalog");

	const schemaVersion = value.schemaVersion;
	const minimumCollectionWindowMinutes = value.minimumCollectionWindowMinutes;
	if (!Number.isInteger(schemaVersion) || schemaVersion !== 1) {
		throw new Error("unsupported baseline signal catalog schema version");
	}
	if (
		!Number.isInteger(minimumCollectionWindowMinutes) ||
		(minimumCollectionWindowMinutes as number) <= 0
	) {
		throw new Error(
			"catalog minimum collection window must be a positive integer",
		);
	}
	if (!Array.isArray(value.signals) || value.signals.length === 0) {
		throw new Error("baseline signal catalog must define signals");
	}

	const deploymentScope = parseDeploymentScope(value.deploymentScope);
	const privacy = parsePrivacyContract(value.privacy);
	const signals = value.signals.map(parseSignal);

  validateCatalogSemantics(deploymentScope, privacy, signals);

	return {
		schemaVersion,
		milestone: requireString(value, "milestone"),
		minimumCollectionWindowMinutes: minimumCollectionWindowMinutes as number,
		deploymentScope,
		privacy,
		signals,
	};
}
