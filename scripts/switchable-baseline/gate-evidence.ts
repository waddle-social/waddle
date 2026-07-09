import {
  fail,
  canonicalArtifactPaths,
  canonicalGateZeroReviewPath,
  canonicalManifestPaths,
  parseRelease,
  parseWindow,
  requireExactKeys,
  requireRecord,
  requireSha256,
  requireString,
  requireUtcInstant,
  requiredRoles,
  releasesEqual,
  sameWindow,
  type ArtifactManifestReference,
  type CapabilityArtifact,
  type CapabilityArtifactRole,
  type EvidenceDeploymentScope,
  type EvidenceWindow,
  type GateZeroArtifactEvidenceKind,
  type GateZeroArtifactManifest,
  type TelemetryArtifact,
  type TelemetryArtifactManifest,
  type TelemetryArtifactRole,
  type ValidatedGateZeroArtifactManifest,
} from "./gate-evidence/common";
import {
  renderEvidenceMarkdown,
} from "./evidence";
import type { SwitchableBaselineEvidence } from "./model";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import {
  LIVE_COLLECTION_BUNDLE_PATH,
  LIVE_COLLECTION_SUBJECT_PATH,
} from "./attestation";
import { validateCapabilityContents } from "./gate-evidence/capability";
import { validateFaroArtifact } from "./gate-evidence/faro";
import {
  readPinnedFile,
  readTrustedJsonSnapshot,
  requireSealedGateZeroEvidenceDirectory,
  requireRepositorySourceAtCommit,
  requireEvidencePath,
  resolveTrustedEvidenceFile,
  type RepositorySourceAtCommitReader,
} from "./gate-evidence/filesystem";
import {
  CAPABILITY_SERVER_SOURCE_PATHS,
  FARO_WEB_SOURCE_PATHS,
  TELEMETRY_SERVER_SOURCE_PATHS,
} from "./source-contract";
import { validatePrometheusArtifact } from "./gate-evidence/prometheus";

export type {
  ArtifactManifestReference,
  CapabilityArtifactManifest,
  EvidenceDeploymentScope,
  EvidenceWindow,
  GateZeroArtifactEvidenceKind,
  GateZeroArtifactManifest,
  TelemetryArtifactManifest,
  ValidatedGateZeroArtifactManifest,
} from "./gate-evidence/common";
export { resolveTrustedEvidenceFile } from "./gate-evidence/filesystem";

export function parseArtifactManifestReference(
  value: unknown,
): ArtifactManifestReference {
  const reference = requireRecord(value, "artifact-manifest reference");
  if (reference.type !== "artifact-manifest") {
    fail(
      "complete Gate 0 capability and telemetry evidence must use an artifact-manifest reference",
    );
  }
  requireExactKeys(
    reference,
    ["type", "path", "sha256"],
    "artifact-manifest reference",
  );
  const path = requireString(
    reference,
    "path",
    "artifact-manifest reference",
  );
  requireEvidencePath(path, "artifact-manifest reference.path");
  if (!path.endsWith(".json")) {
    fail("artifact-manifest reference.path must name a JSON file");
  }
  return {
    type: "artifact-manifest",
    path,
    sha256: requireSha256(
      reference.sha256,
      "artifact-manifest reference.sha256",
    ),
  };
}

function parseArtifact(
  value: unknown,
  evidenceKind: GateZeroArtifactEvidenceKind,
  manifestRelease: { serverCommit: string; webCommit: string },
  manifestWindow: EvidenceWindow,
  index: number,
): CapabilityArtifact | TelemetryArtifact {
  const label = "artifact-manifest.artifacts[" + index + "]";
  const artifact = requireRecord(value, label);
  const keys = ["role", "path", "sha256", "release", "window"];
  requireExactKeys(artifact, keys, label);

  const role = requireString(artifact, "role", label);
  if (!requiredRoles[evidenceKind].includes(role)) {
    fail(label + ".role is not valid for " + evidenceKind + ": " + role);
  }
  const path = requireString(artifact, "path", label);
  requireEvidencePath(path, label + ".path");
  if (!path.endsWith(".json")) {
    fail(label + ".path must name a normalized JSON artifact");
  }
  if (path !== canonicalArtifactPaths[evidenceKind][role]) {
    fail(label + ".path must use the canonical path for role " + role);
  }
  const sha256 = requireSha256(artifact.sha256, label + ".sha256");
  const release = parseRelease(artifact.release, label + ".release");
  if (!releasesEqual(release, manifestRelease)) {
    fail(label + ".release must match artifact-manifest.release");
  }
  const window = parseWindow(artifact.window, label + ".window");
  if (!sameWindow(window, manifestWindow)) {
    fail(label + ".window must match the shared Gate 0 collection window");
  }

  if (evidenceKind === "telemetry-baseline") {
    return {
      role: role as TelemetryArtifactRole,
      path,
      sha256,
      release,
      window,
    };
  }
  return {
    role: role as CapabilityArtifactRole,
    path,
    sha256,
    release,
    window,
  };
}

function parseArtifactManifest(
  value: unknown,
  expectedKind: GateZeroArtifactEvidenceKind,
): GateZeroArtifactManifest {
  const manifest = requireRecord(value, "artifact-manifest");
  const keys = [
    "schemaVersion",
    "evidenceKind",
    "status",
    "release",
    "window",
    "capturedAt",
    "artifacts",
  ];
  requireExactKeys(manifest, keys, "artifact-manifest");
  if (manifest.schemaVersion !== 1) {
    fail("artifact-manifest.schemaVersion must be 1");
  }
  if (manifest.evidenceKind !== expectedKind) {
    fail("artifact-manifest.evidenceKind must be " + expectedKind);
  }
  if (manifest.status !== "complete") {
    fail("artifact-manifest.status must be complete");
  }
  const release = parseRelease(manifest.release, "artifact-manifest.release");
  const capturedAt = requireUtcInstant(
    manifest.capturedAt,
    "artifact-manifest.capturedAt",
  );
  const window = parseWindow(manifest.window, "artifact-manifest.window");
  if (
    Date.parse(window.end) - Date.parse(window.start) < 60 * 60 * 1_000
  ) {
    fail("artifact-manifest.window must span at least 60 minutes");
  }
  if (Date.parse(capturedAt) < Date.parse(window.end)) {
    fail(
      "artifact-manifest.capturedAt must not be earlier than the collection window end",
    );
  }
  if (!Array.isArray(manifest.artifacts)) {
    fail("artifact-manifest.artifacts must be an array");
  }
  const artifacts = manifest.artifacts.map((artifact, index) =>
    parseArtifact(artifact, expectedKind, release, window, index)
  );
  const roles = artifacts.map(({ role }) => role);
  const paths = artifacts.map(({ path }) => path);
  if (new Set(roles).size !== roles.length) {
    fail("artifact-manifest artifact roles must be unique");
  }
  if (new Set(paths).size !== paths.length) {
    fail("artifact-manifest artifact paths must be unique");
  }
  const expectedRoles = [...requiredRoles[expectedKind]].sort();
  if (JSON.stringify([...roles].sort()) !== JSON.stringify(expectedRoles)) {
    fail(
      expectedKind + " requires exactly these artifact roles: "
        + expectedRoles.join(", "),
    );
  }

  if (expectedKind === "telemetry-baseline") {
    return {
      schemaVersion: 1,
      evidenceKind: expectedKind,
      status: "complete",
      release,
      capturedAt,
      window: window as EvidenceWindow,
      artifacts: artifacts as TelemetryArtifact[],
    };
  }
  return {
    schemaVersion: 1,
    evidenceKind: expectedKind,
    status: "complete",
    release,
    window,
    capturedAt,
    artifacts: artifacts as CapabilityArtifact[],
  };
}

function validateTelemetryContents(
  repositoryRoot: string,
  manifest: TelemetryArtifactManifest,
  contents: Map<TelemetryArtifactRole, unknown>,
): { scope: EvidenceDeploymentScope; catalogSha256: string } {
  const prometheusArtifact = manifest.artifacts.find(
    ({ role }) => role === "prometheus-baseline",
  );
  if (!prometheusArtifact) {
    fail("telemetry artifacts must include prometheus-baseline");
  }
  const validated = validatePrometheusArtifact(
    repositoryRoot,
    contents.get("prometheus-baseline"),
    prometheusArtifact,
  );
  for (const artifact of manifest.artifacts) {
    if (artifact.role === "prometheus-baseline") continue;
    validateFaroArtifact(
      contents.get(artifact.role),
      artifact,
      validated.catalog,
      validated.scope,
    );
  }
  return {
    scope: validated.scope,
    catalogSha256: validated.catalog.sha256,
  };
}

export async function validateArtifactManifestReference(
  repositoryRoot: string,
  value: unknown,
  expectedKind: GateZeroArtifactEvidenceKind,
  sourceAtCommit?: RepositorySourceAtCommitReader,
): Promise<ValidatedGateZeroArtifactManifest> {
  const reference = parseArtifactManifestReference(value);
  if (reference.path !== canonicalManifestPaths[expectedKind]) {
    fail("artifact-manifest reference.path must use the canonical path for " + expectedKind);
  }
  const manifestSnapshot = readTrustedJsonSnapshot(
    repositoryRoot,
    reference.path,
    "docs/evidence",
    "artifact-manifest reference.path",
    ".json",
    reference.sha256,
  );
  if (manifestSnapshot.sha256 !== reference.sha256) {
    fail(
      "artifact-manifest reference SHA-256 does not match the manifest bytes",
    );
  }
  const manifest = parseArtifactManifest(
    manifestSnapshot.value,
    expectedKind,
  );

  const serverSourcePaths = expectedKind === "telemetry-baseline"
    ? TELEMETRY_SERVER_SOURCE_PATHS
    : CAPABILITY_SERVER_SOURCE_PATHS;
  for (const sourcePath of serverSourcePaths) {
    await requireRepositorySourceAtCommit(
      repositoryRoot,
      manifest.release.serverCommit,
      sourcePath,
      expectedKind + " source",
      sourceAtCommit,
    );
  }
  if (expectedKind === "telemetry-baseline") {
    for (const sourcePath of FARO_WEB_SOURCE_PATHS) {
      await requireRepositorySourceAtCommit(
        repositoryRoot,
        manifest.release.webCommit,
        sourcePath,
        "telemetry-baseline web source",
        sourceAtCommit,
      );
    }
  }

  const contents = new Map<string, unknown>();
  for (const [index, artifact] of manifest.artifacts.entries()) {
    if (artifact.path === reference.path) {
      fail(
        "artifact-manifest.artifacts[" + index
          + "] must not reference the manifest itself",
      );
    }
    const artifactSnapshot = readTrustedJsonSnapshot(
      repositoryRoot,
      artifact.path,
      "docs/evidence",
      "artifact-manifest.artifacts[" + index + "].path",
      ".json",
      artifact.sha256,
    );
    if (artifactSnapshot.sha256 !== artifact.sha256) {
      fail(
        "artifact-manifest.artifacts[" + index
          + "] SHA-256 does not match the artifact bytes",
      );
    }
    contents.set(
      artifact.role,
      artifactSnapshot.value,
    );
  }

  if (manifest.evidenceKind === "capability-baseline") {
    const scope = validateCapabilityContents(
      repositoryRoot,
      manifest,
      contents as Map<CapabilityArtifactRole, unknown>,
    );
    return Object.assign(manifest, {
      deploymentScope: scope,
      repositoryRoot,
      referencePath: reference.path,
    });
  }
  const validated = validateTelemetryContents(
    repositoryRoot,
    manifest,
    contents as Map<TelemetryArtifactRole, unknown>,
  );
  return Object.assign(manifest, {
    deploymentScope: validated.scope,
    catalogSha256: validated.catalogSha256,
    repositoryRoot,
    referencePath: reference.path,
  });
}

export function requireSharedGateZeroRelease(
  manifests: readonly ValidatedGateZeroArtifactManifest[],
): void {
  if (manifests.length < 2) return;
  const releases = new Set(manifests.map(({ release }) => JSON.stringify(release)));
  if (releases.size !== 1) {
    fail(
      "complete Gate 0 capability and telemetry manifests must share one release tuple",
    );
  }
  const scopes = new Set(
    manifests.map(({ deploymentScope }) => JSON.stringify(deploymentScope)),
  );
  if (scopes.size !== 1) {
    fail(
      "complete Gate 0 capability and telemetry manifests must share one deployment scope",
    );
  }
  const windows = new Set(manifests.map(({ window }) => JSON.stringify(window)));
  if (windows.size !== 1) {
    fail(
      "complete Gate 0 capability and telemetry manifests must share one collection window",
    );
  }
  const roots = new Set(manifests.map(({ repositoryRoot }) => repositoryRoot));
  if (roots.size !== 1) {
    fail("complete Gate 0 manifests must come from one repository root");
  }
  const kinds = manifests.map(({ evidenceKind }) => evidenceKind).sort();
  if (
    JSON.stringify(kinds)
    === JSON.stringify(["capability-baseline", "telemetry-baseline"])
  ) {
    const telemetry = manifests.find(
      ({ evidenceKind }) => evidenceKind === "telemetry-baseline",
    );
    if (!telemetry) fail("complete Gate 0 manifests require telemetry evidence");
    const prometheus = telemetry.artifacts.find(
      ({ role }) => role === "prometheus-baseline",
    );
    if (!prometheus) fail("complete Gate 0 telemetry requires Prometheus evidence");
    const reviewPath = resolveTrustedEvidenceFile(
      telemetry.repositoryRoot,
      canonicalGateZeroReviewPath,
      "generated Gate 0 review",
      ".md",
    );
    const evidenceSnapshot = readTrustedJsonSnapshot(
      telemetry.repositoryRoot,
      prometheus.path,
      "docs/evidence",
      "prometheus-baseline review source",
      ".json",
    );
    if (evidenceSnapshot.sha256 !== prometheus.sha256) {
      fail("prometheus-baseline review source SHA-256 does not match its manifest");
    }
    const evidence = evidenceSnapshot.value as SwitchableBaselineEvidence;
    const expectedReview = renderEvidenceMarkdown(evidence, prometheus.sha256);
    if (readPinnedFile(reviewPath, "generated Gate 0 review").bytes.toString("utf8") !== expectedReview) {
      fail("generated Gate 0 review must exactly match the validated Prometheus artifact");
    }
    const attestationPaths = [
      LIVE_COLLECTION_SUBJECT_PATH,
      LIVE_COLLECTION_BUNDLE_PATH,
    ];
    const presentAttestationPaths = attestationPaths.filter((path) =>
      existsSync(resolve(telemetry.repositoryRoot, path))
    );
    requireSealedGateZeroEvidenceDirectory(
      telemetry.repositoryRoot,
      presentAttestationPaths.length === attestationPaths.length
        ? attestationPaths
        : [],
    );
  }
}

export function requireArtifactManifestForCompleteGateZeroRecord(record: {
  kind: string;
  status: string;
  reference: unknown;
}): ArtifactManifestReference | undefined {
  if (
    record.status !== "complete"
    || !["capability-baseline", "telemetry-baseline"].includes(record.kind)
  ) {
    return undefined;
  }
  const reference = parseArtifactManifestReference(record.reference);
  const expectedKind = record.kind as GateZeroArtifactEvidenceKind;
  if (reference.path !== canonicalManifestPaths[expectedKind]) {
    fail("artifact-manifest reference.path must use the canonical path for " + expectedKind);
  }
  return reference;
}
