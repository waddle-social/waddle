import {
  fail,
  parseDeploymentScope,
  requireCommit,
  requireExactKeys,
  requireRecord,
  type EvidenceDeploymentScope,
  type TelemetryArtifact,
} from "./common";
import {
  parseCatalogReference,
  type TrustedCatalog,
} from "./catalog";
import { validateAutomatedPrometheus } from "./prometheus/automated";
import { validateCollectionWindow } from "./prometheus/window";
import { validateManualFaroRequirement } from "./prometheus/manual";

export interface ValidatedPrometheusArtifact {
  scope: EvidenceDeploymentScope;
  catalog: TrustedCatalog;
}

export function validatePrometheusArtifact(
  repositoryRoot: string,
  value: unknown,
  artifact: TelemetryArtifact,
): ValidatedPrometheusArtifact {
  const label = "prometheus-baseline artifact";
  const evidence = requireRecord(value, label);
  requireExactKeys(evidence, [
    "schemaVersion",
    "artifactRole",
    "evidenceKind",
    "milestone",
    "gate",
    "status",
    "gateReadiness",
    "serverCommit",
    "deploymentScope",
    "catalog",
    "collectionWindow",
    "automatedPrometheus",
    "manualFaro",
    "conclusion",
  ], label);
  if (evidence.schemaVersion !== 1) {
    fail(label + ".schemaVersion must be 1");
  }
  if (evidence.artifactRole !== artifact.role) {
    fail(label + ".artifactRole must match its manifest role");
  }
  if (evidence.evidenceKind !== "gate-0-switchable-baseline") {
    fail(label + ".evidenceKind must be gate-0-switchable-baseline");
  }
  if (
    evidence.milestone !== "switchable-alternative"
    || evidence.gate !== 0
  ) {
    fail(label + " must identify the switchable-alternative Gate 0 program");
  }
  if (
    evidence.status !== "partial"
    || evidence.gateReadiness !== "not-ready"
  ) {
    fail(label + " must remain partial until the Faro artifacts are reviewed");
  }
  if (
    requireCommit(evidence.serverCommit, label + ".serverCommit")
    !== artifact.release.serverCommit
  ) {
    fail(label + ".serverCommit must match its artifact release");
  }
  const scope = parseDeploymentScope(
    evidence.deploymentScope,
    label + ".deploymentScope",
  );
  const catalog = parseCatalogReference(
    repositoryRoot,
    evidence.catalog,
    label + ".catalog",
  );
  if (
    scope.identityMetric !== catalog.deploymentScope.identityMetric
    || scope.targetSignalId !== catalog.deploymentScope.targetSignalId
  ) {
    fail(label + ".deploymentScope identity fields must match the catalog");
  }
  if (
    scope.identityLookbackSeconds
    !== catalog.deploymentScope.maximumRangeLookbackSeconds
  ) {
    fail(
      label
        + ".deploymentScope.identityLookbackSeconds must match the catalog maximum lookback",
    );
  }


  const { parsedWindow, stepSeconds } = validateCollectionWindow(
    evidence.collectionWindow,
    artifact.window,
    catalog,
    label,
  );
  validateAutomatedPrometheus(
    evidence.automatedPrometheus,
    catalog,
    scope,
    artifact.release.serverCommit,
    parsedWindow,
    stepSeconds,
    label,
  );
  validateManualFaroRequirement(evidence.manualFaro, evidence.conclusion, label);
  return { scope, catalog };
}
