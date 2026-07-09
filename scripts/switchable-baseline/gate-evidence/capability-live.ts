import {
  fail,
  parseDeploymentScope,
  parseWindow,
  requireCommit,
  requireExactKeys,
  requireRecord,
  requireSha256,
  requireString,
  requireUtcInstant,
  sameWindow,
  type CapabilityArtifact,
  type EvidenceDeploymentScope,
} from "./common";
import {
  requireBoundedDiscoName,
  requirePrivacySafeFeature,
  requireUniqueStrings,
  type DiscoTargetContract,
  type TargetContractEntry,
} from "./capability-contract";

/** Privacy-safe result produced by the native XMPP discovery collector. */

export interface LiveDiscoEvidence {
  scope: EvidenceDeploymentScope;
  capturedAt: string;
  featuresByTarget: Map<string, Set<string>>;
  skippedTargets: Set<string>;
  targetCount: number;
}
function validateObservedFeatures(
  target: TargetContractEntry,
  observed: string[],
  label: string,
  contract: DiscoTargetContract,
): void {
  const actual = new Set(observed);
  const claimable = new Set(target.claimableFeatures);
  for (const feature of observed.filter((entry) => !claimable.has(entry))) {
    const otherOwners = contract.targets
      .filter(({ slug, claimableFeatures }) =>
        slug !== target.slug && claimableFeatures.includes(feature)
      )
      .map(({ slug }) => slug);
    if (otherOwners.length > 0) {
      fail(
        label + " must not contain a synthetic union feature owned by "
          + otherOwners.join(", "),
      );
    }
    fail(
      label + " contains a feature outside the exact checked-in target registry: "
      + feature,
    );
  }
  const optional = new Set(target.independentlyOptionalFeatures);
  const base = observed.filter((feature) => !optional.has(feature));
  const baseKey = JSON.stringify([...base].sort());
  if (target.observationPolicy === "runtime-dependent") {
    const matchesVariant = target.runtimeFeatureVariants.some((variant) =>
      JSON.stringify([...variant].sort()) === baseKey
    );
    if (!matchesVariant) fail(label + " must match one complete runtime feature variant");
    return;
  }
  if (JSON.stringify([...target.requiredFeatures].sort()) !== baseKey) {
    fail(label + " must match the required feature baseline plus curated optional features");
  }
}

export function validateLiveDiscoArtifact(
  value: unknown,
  artifact: CapabilityArtifact,
  contract: DiscoTargetContract,
): LiveDiscoEvidence {
  const label = "live-disco-export artifact";
  const evidence = requireRecord(value, label);
  requireExactKeys(evidence, [
    "schemaVersion",
    "artifactRole",
    "evidenceKind",
    "status",
    "serverCommit",
    "capturedAt",
    "window",
    "deploymentScope",
    "targetContractSha256",
    "entities",
    "skippedTargets",
  ], label);
  if (evidence.schemaVersion !== 1) fail(label + ".schemaVersion must be 1");
  if (evidence.artifactRole !== artifact.role) {
    fail(label + ".artifactRole must match its manifest role");
  }
  if (evidence.evidenceKind !== "gate-0-capability-live-disco") {
    fail(label + ".evidenceKind must be gate-0-capability-live-disco");
  }
  if (evidence.status !== "collected") fail(label + ".status must be collected");
  if (
    requireCommit(evidence.serverCommit, label + ".serverCommit")
    !== artifact.release.serverCommit
  ) fail(label + ".serverCommit must match its artifact release");
  if (
    requireSha256(evidence.targetContractSha256, label + ".targetContractSha256")
    !== contract.sha256
  ) fail(label + ".targetContractSha256 must bind the trusted target contract");
  const capturedAt = requireUtcInstant(evidence.capturedAt, label + ".capturedAt");
  const window = parseWindow(evidence.window, label + ".window");
  if (!sameWindow(window, artifact.window)) {
    fail(label + ".window must match its artifact manifest entry");
  }
  if (
    Date.parse(capturedAt) < Date.parse(artifact.window.start)
    || Date.parse(capturedAt) > Date.parse(artifact.window.end)
  ) fail(label + ".capturedAt must fall inside the shared Gate 0 window");
  const scope = parseDeploymentScope(
    evidence.deploymentScope,
    label + ".deploymentScope",
  );
  if (!Array.isArray(evidence.entities)) fail(label + ".entities must be an array");
  if (!Array.isArray(evidence.skippedTargets)) {
    fail(label + ".skippedTargets must be an array");
  }

  const featuresByTarget = new Map<string, Set<string>>();
  const entityOrder: string[] = [];
  for (const [index, entry] of evidence.entities.entries()) {
    const entityLabel = label + ".entities[" + index + "]";
    const entity = requireRecord(entry, entityLabel);
    requireExactKeys(entity, ["target", "identities", "features"], entityLabel);
    const targetSlug = requireString(entity, "target", entityLabel);
    const target = contract.bySlug.get(targetSlug);
    if (!target || featuresByTarget.has(targetSlug)) {
      fail(entityLabel + ".target must be a unique canonical target");
    }
    entityOrder.push(targetSlug);
    if (!Array.isArray(entity.identities)) {
      fail(entityLabel + ".identities must be an array");
    }
    const identities = entity.identities.map((identity, identityIndex) => {
      const identityLabel = entityLabel + ".identities[" + identityIndex + "]";
      const record = requireRecord(identity, identityLabel);
      requireExactKeys(record, ["category", "type"], identityLabel);
      return {
        category: requireBoundedDiscoName(record.category, identityLabel + ".category"),
        type: requireBoundedDiscoName(record.type, identityLabel + ".type"),
      };
    });
    if (JSON.stringify(identities) !== JSON.stringify(target.identities)) {
      fail(entityLabel + ".identities must match the target contract without names");
    }
    const features = requireUniqueStrings(entity.features, entityLabel + ".features");
    if (JSON.stringify(features) !== JSON.stringify([...features].sort())) {
      fail(entityLabel + ".features must be sorted");
    }
    features.forEach((feature, featureIndex) =>
      requirePrivacySafeFeature(feature, entityLabel + ".features[" + featureIndex + "]")
    );
    validateObservedFeatures(
      target,
      features,
      entityLabel + ".features",
      contract,
    );
    featuresByTarget.set(targetSlug, new Set(features));
  }

  const skippedTargets = new Set<string>();
  const skippedOrder: string[] = [];
  for (const [index, entry] of evidence.skippedTargets.entries()) {
    const skipLabel = label + ".skippedTargets[" + index + "]";
    const skip = requireRecord(entry, skipLabel);
    requireExactKeys(skip, ["target", "reason"], skipLabel);
    const targetSlug = requireString(skip, "target", skipLabel);
    const target = contract.bySlug.get(targetSlug);
    if (!target || skippedTargets.has(targetSlug) || featuresByTarget.has(targetSlug)) {
      fail(skipLabel + ".target must be a unique unobserved canonical target");
    }
    if (target.availability === "always") {
      fail(skipLabel + ".target cannot skip an always-available target");
    }
    const expectedReason = target.availability === "configured"
      ? "not-configured"
      : "no-representative-entity";
    if (skip.reason !== expectedReason) {
      fail(skipLabel + ".reason must match the target availability");
    }
    skippedTargets.add(targetSlug);
    skippedOrder.push(targetSlug);
  }
  const accountedOrder = contract.targets
    .map(({ slug }) => slug)
    .filter((slug) => featuresByTarget.has(slug) || skippedTargets.has(slug));
  const contractSlugs = contract.targets.map(({ slug }) => slug);
  if (
    JSON.stringify([...entityOrder, ...skippedOrder].sort())
    !== JSON.stringify([...contractSlugs].sort())
    || JSON.stringify(entityOrder)
      !== JSON.stringify(accountedOrder.filter((slug) => featuresByTarget.has(slug)))
    || JSON.stringify(skippedOrder)
      !== JSON.stringify(accountedOrder.filter((slug) => skippedTargets.has(slug)))
  ) fail(label + " must account for every canonical target in contract order");
  return {
    scope,
    capturedAt,
    featuresByTarget,
    skippedTargets,
    targetCount: featuresByTarget.size,
  };
}
