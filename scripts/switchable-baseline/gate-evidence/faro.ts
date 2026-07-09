import {
  materializeFaroQueryPlan,
  validateFaroSeries,
} from "../faro";
import {
  fail,
  faroRoleSignalIds,
  parseRelease,
  parseWindow,
  requireExactKeys,
  requireInteger,
  requireRecord,
  requireSha256,
  requireSortedExactStrings,
  releasesEqual,
  sameWindow,
  type EvidenceDeploymentScope,
  type TelemetryArtifact,
} from "./common";
import type { TrustedCatalog } from "./catalog";

export function validateFaroArtifact(
  value: unknown,
  artifact: TelemetryArtifact,
  catalog: TrustedCatalog,
  prometheusScope: EvidenceDeploymentScope,
): void {
  if (artifact.role === "prometheus-baseline") fail("internal Faro role mismatch");
  const label = artifact.role + " artifact";
  const evidence = requireRecord(value, label);
  requireExactKeys(evidence, [
    "schemaVersion",
    "evidenceKind",
    "role",
    "signalId",
    "release",
    "window",
    "scope",
    "source",
    "dimensions",
    "series",
  ], label);
  if (evidence.schemaVersion !== 1) fail(label + ".schemaVersion must be 1");
  if (evidence.role !== artifact.role) fail(label + ".role must match its manifest role");
  if (evidence.evidenceKind !== "gate-0-faro-aggregate") {
    fail(label + ".evidenceKind must be gate-0-faro-aggregate");
  }
  const expectedSignalId = faroRoleSignalIds[artifact.role];
  if (evidence.signalId !== expectedSignalId) {
    fail(label + ".signalId must match its artifact role");
  }
  const release = parseRelease(evidence.release, label + ".release");
  if (!releasesEqual(release, artifact.release)) {
    fail(label + ".release must match its artifact manifest entry");
  }
  const window = parseWindow(evidence.window, label + ".window");
  if (!sameWindow(window, artifact.window)) {
    fail(label + ".window must match its artifact manifest entry");
  }

  const scope = requireRecord(evidence.scope, label + ".scope");
  requireExactKeys(scope, [
    "sourceId",
    "deploymentEnvironment",
    "release",
    "cluster",
    "namespace",
  ], label + ".scope");
  if (scope.sourceId !== "waddle-chat") {
    fail(label + ".scope.sourceId must be waddle-chat");
  }
  for (const [key, expected] of Object.entries({
    deploymentEnvironment: prometheusScope.environment,
    release: artifact.release.webCommit,
    cluster: prometheusScope.cluster,
    namespace: prometheusScope.namespace,
  })) {
    if (scope[key] !== expected) fail(label + ".scope." + key + " must match Prometheus scope");
  }

  const signal = catalog.signals.find(({ id }) => id === expectedSignalId);
  if (!signal || signal.source !== "faro" || signal.collection !== "manual-export") {
    fail(label + " must reference a catalogued Faro manual-export signal");
  }
  const source = requireRecord(evidence.source, label + ".source");
  requireExactKeys(
    source,
    ["sourceId", "query", "rawSha256", "rowCount"],
    label + ".source",
  );
  const expectedQuery = materializeFaroQueryPlan(signal, {
    webCommit: artifact.release.webCommit,
    deploymentEnvironment: prometheusScope.environment,
    cluster: prometheusScope.cluster,
    namespace: prometheusScope.namespace,
    window: artifact.window,
  });
  if (JSON.stringify(source.query) !== JSON.stringify(expectedQuery)) {
    fail(label + ".source.query must match the exact catalog release/environment/window plan");
  }
  if (
    source.sourceId !== expectedQuery.sourceId
    || scope.sourceId !== source.sourceId
  ) {
    fail(label + ".source.sourceId and scope.sourceId must derive from the validated query");
  }
  requireSha256(source.rawSha256, label + ".source.rawSha256");

  const dimensions = requireRecord(evidence.dimensions, label + ".dimensions");
  requireExactKeys(dimensions, Object.keys(signal.attributes), label + ".dimensions");
  for (const [key, values] of Object.entries(signal.attributes)) {
    requireSortedExactStrings(dimensions[key], values, label + ".dimensions." + key);
  }
  try {
    validateFaroSeries(signal, evidence.series);
  } catch (error) {
    fail(error instanceof Error ? error.message : label + ".series is invalid");
  }
  const rowCount = requireInteger(source.rowCount, label + ".source.rowCount", 1);
  if (!Array.isArray(evidence.series) || rowCount !== evidence.series.length) {
    fail(label + ".source.rowCount must equal the normalized aggregate series count");
  }
}
