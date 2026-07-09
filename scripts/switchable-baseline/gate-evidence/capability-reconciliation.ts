import {
  fail,
  parseDeploymentScope,
  requireCommit,
  requireExactKeys,
  requireRecord,
  requireSha256,
  requireSortedExactStrings,
  requireString,
  requireUtcInstant,
  scopesEqual,
  type CapabilityArtifact,
  type EvidenceDeploymentScope,
} from "./common";
import {
  requirePrivacySafeFeature,
  requireUniqueStrings,
  type DiscoTargetContract,
} from "./capability-contract";
import type { LiveDiscoEvidence } from "./capability-live";
import {
  readPinnedFile,
  resolveTrustedRepositoryFile,
} from "./filesystem";
import { compareText } from "../model";

/** Deterministic comparison of declarations, target contracts, and live disco. */

export interface CapabilityDeclaration {
  id: string;
  targets: Map<string, string[]>;
}

function parseTargetFeatureMap(
  value: unknown,
  label: string,
  contract: DiscoTargetContract,
): Map<string, string[]> {
  const record = requireRecord(value, label);
  const result = new Map<string, string[]>();
  for (const targetSlug of Object.keys(record).sort()) {
    const target = contract.bySlug.get(targetSlug);
    if (!target) fail(label + " uses unknown target " + targetSlug);
    const features = requireUniqueStrings(record[targetSlug], label + "." + targetSlug);
    if (features.length === 0) fail(label + "." + targetSlug + " must not be empty");
    for (const feature of features) {
      requirePrivacySafeFeature(feature, label + "." + targetSlug);
      if (!target.claimableFeatures.includes(feature)) {
        fail(label + "." + targetSlug + " claims a feature not owned by that target: " + feature);
      }
    }
    result.set(targetSlug, [...features].sort());
  }
  return result;
}

export function loadCapabilityDeclarations(
  repositoryRoot: string,
  value: unknown,
  label: string,
  contract: DiscoTargetContract,
): CapabilityDeclaration[] {
  const reference = requireRecord(value, label);
  requireExactKeys(reference, ["path", "sha256", "schemaVersion"], label);
  const path = requireString(reference, "path", label);
  if (path !== "server/capabilities.toml") {
    fail(label + ".path must be server/capabilities.toml");
  }
  if (reference.schemaVersion !== 1) fail(label + ".schemaVersion must be 1");
  const expectedSha = requireSha256(reference.sha256, label + ".sha256");
  const manifestPath = resolveTrustedRepositoryFile(
    repositoryRoot,
    path,
    "server",
    label + ".path",
  );
  const manifestSnapshot = readPinnedFile(manifestPath, label + ".path");
  if (manifestSnapshot.sha256 !== expectedSha) {
    fail(label + ".sha256 does not match the capability manifest bytes");
  }
  let parsed: unknown;
  try {
    parsed = Bun.TOML.parse(manifestSnapshot.bytes.toString("utf8"));
  } catch {
    fail(label + ".path does not contain valid TOML");
  }
  const manifest = requireRecord(parsed, "capability manifest");
  if (manifest.schema_version !== 1 || !Array.isArray(manifest.capability)) {
    fail("capability manifest must use schema_version 1 and contain capabilities");
  }
  const declarations = manifest.capability.map((entry, index): CapabilityDeclaration => {
    const capabilityLabel = "capability manifest.capability[" + index + "]";
    const capability = requireRecord(entry, capabilityLabel);
    const advertised = parseTargetFeatureMap(
      capability.advertised_features,
      capabilityLabel + ".advertised_features",
      contract,
    );
    const custom = capability.custom_namespaces === undefined
      ? new Map<string, string[]>()
      : parseTargetFeatureMap(
        capability.custom_namespaces,
        capabilityLabel + ".custom_namespaces",
        contract,
      );
    const targets = new Map<string, string[]>();
    for (const targetSlug of new Set([...advertised.keys(), ...custom.keys()])) {
      const combined = [
        ...(advertised.get(targetSlug) ?? []),
        ...(custom.get(targetSlug) ?? []),
      ].sort();
      if (new Set(combined).size !== combined.length) {
        fail(capabilityLabel + " repeats a target-local feature claim");
      }
      targets.set(targetSlug, combined);
    }
    if (targets.size === 0) fail(capabilityLabel + " must declare target-local features");
    return {
      id: requireString(capability, "id", capabilityLabel),
      targets: new Map([...targets].sort(([left], [right]) => compareText(left, right))),
    };
  }).sort((left, right) => compareText(left.id, right.id));
  if (new Set(declarations.map(({ id }) => id)).size !== declarations.length) {
    fail("capability manifest capability ids must be unique");
  }
  return declarations;
}

export function validateReconciliationArtifact(
  repositoryRoot: string,
  value: unknown,
  artifact: CapabilityArtifact,
  liveArtifact: CapabilityArtifact,
  live: LiveDiscoEvidence,
  contract: DiscoTargetContract,
): { scope: EvidenceDeploymentScope; capturedAt: string } {
  const label = "capability-reconciliation artifact";
  const evidence = requireRecord(value, label);
  requireExactKeys(evidence, [
    "schemaVersion",
    "artifactRole",
    "evidenceKind",
    "status",
    "serverCommit",
    "capturedAt",
    "deploymentScope",
    "targetContractSha256",
    "liveDiscoSha256",
    "capabilityManifest",
    "summary",
    "checks",
  ], label);
  if (evidence.schemaVersion !== 1) fail(label + ".schemaVersion must be 1");
  if (evidence.artifactRole !== artifact.role) {
    fail(label + ".artifactRole must match its manifest role");
  }
  if (evidence.evidenceKind !== "gate-0-capability-reconciliation") {
    fail(label + ".evidenceKind must be gate-0-capability-reconciliation");
  }
  if (evidence.status !== "matched") fail(label + ".status must be matched");
  if (
    requireCommit(evidence.serverCommit, label + ".serverCommit")
    !== artifact.release.serverCommit
  ) fail(label + ".serverCommit must match its artifact release");
  if (
    requireSha256(evidence.targetContractSha256, label + ".targetContractSha256")
    !== contract.sha256
  ) fail(label + ".targetContractSha256 must bind the trusted target contract");
  const capturedAt = requireUtcInstant(evidence.capturedAt, label + ".capturedAt");
  if (
    Date.parse(capturedAt) < Date.parse(artifact.window.start)
    || Date.parse(capturedAt) > Date.parse(artifact.window.end)
  ) fail(label + ".capturedAt must fall inside the shared Gate 0 window");
  if (Date.parse(capturedAt) < Date.parse(live.capturedAt)) {
    fail(label + ".capturedAt must not precede live disco collection");
  }
  const scope = parseDeploymentScope(
    evidence.deploymentScope,
    label + ".deploymentScope",
  );
  if (!scopesEqual(scope, live.scope)) {
    fail(label + ".deploymentScope must match live disco");
  }
  if (
    requireSha256(evidence.liveDiscoSha256, label + ".liveDiscoSha256")
    !== liveArtifact.sha256
  ) fail(label + ".liveDiscoSha256 must bind the live disco artifact bytes");
  const declarations = loadCapabilityDeclarations(
    repositoryRoot,
    evidence.capabilityManifest,
    label + ".capabilityManifest",
    contract,
  );
  const summary = requireRecord(evidence.summary, label + ".summary");
  requireExactKeys(summary, [
    "declaredCapabilityCount",
    "observedTargetCount",
    "missingAdvertisedFeatures",
    "unexpectedOfficialFeatures",
    "capabilityMismatches",
  ], label + ".summary");
  if (summary.declaredCapabilityCount !== declarations.length) {
    fail(label + ".summary.declaredCapabilityCount must match the capability manifest");
  }
  if (summary.observedTargetCount !== live.targetCount) {
    fail(label + ".summary.observedTargetCount must match live disco");
  }
  for (const key of [
    "missingAdvertisedFeatures",
    "unexpectedOfficialFeatures",
    "capabilityMismatches",
  ] as const) {
    if (!Array.isArray(summary[key]) || summary[key].length !== 0) {
      fail(label + ".summary." + key + " must be empty for matched evidence");
    }
  }
  if (!Array.isArray(evidence.checks)) fail(label + ".checks must be an array");
  const expectedIds = declarations.map(({ id }) => id);
  const actualIds = evidence.checks.map((entry, index) =>
    requireString(
      requireRecord(entry, label + ".checks[" + index + "]"),
      "capabilityId",
      label,
    )
  );
  if (JSON.stringify(actualIds) !== JSON.stringify(expectedIds)) {
    fail(label + ".checks must cover every capability in sorted order");
  }
  evidence.checks.forEach((entry, index) => {
    const checkLabel = label + ".checks[" + index + "]";
    const check = requireRecord(entry, checkLabel);
    requireExactKeys(check, ["capabilityId", "status", "targets"], checkLabel);
    if (check.status !== "matched") fail(checkLabel + ".status must be matched");
    if (!Array.isArray(check.targets)) fail(checkLabel + ".targets must be an array");
    const declaration = declarations[index];
    const expectedTargets = [...declaration.targets.keys()];
    const actualTargets = check.targets.map((targetEntry, targetIndex) =>
      requireString(
        requireRecord(targetEntry, checkLabel + ".targets[" + targetIndex + "]"),
        "target",
        checkLabel,
      )
    );
    if (JSON.stringify(actualTargets) !== JSON.stringify(expectedTargets)) {
      fail(checkLabel + ".targets must cover the exact declared target set");
    }
    check.targets.forEach((targetEntry, targetIndex) => {
      const targetLabel = checkLabel + ".targets[" + targetIndex + "]";
      const targetCheck = requireRecord(targetEntry, targetLabel);
      requireExactKeys(
        targetCheck,
        ["target", "declaredFeatures", "observedFeatures"],
        targetLabel,
      );
      const targetSlug = actualTargets[targetIndex];
      const declared = declaration.targets.get(targetSlug) ?? [];
      requireSortedExactStrings(
        targetCheck.declaredFeatures,
        declared,
        targetLabel + ".declaredFeatures",
      );
      requireSortedExactStrings(
        targetCheck.observedFeatures,
        declared,
        targetLabel + ".observedFeatures",
      );
      const liveFeatures = live.featuresByTarget.get(targetSlug);
      if (!liveFeatures) {
        fail(targetLabel + " cannot match a skipped or missing live target");
      }
      for (const feature of declared) {
        if (!liveFeatures.has(feature)) {
          fail(targetLabel + ".observedFeatures must come from its declared live target");
        }
      }
    });
  });
  for (const declaration of declarations) {
    for (const targetSlug of declaration.targets.keys()) {
      if (live.skippedTargets.has(targetSlug)) {
        fail("manifest-declared target " + targetSlug + " must not be skipped");
      }
    }
  }
  return { scope, capturedAt };
}
