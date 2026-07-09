import {
  fail,
  kebabPattern,
  requireExactKeys,
  requireRecord,
  requireString,
  requireStringArray,
  type CapabilityArtifact,
} from "./common";
import {
  readPinnedFile,
  resolveTrustedRepositoryFile,
} from "./filesystem";

/** Trusted, versioned contract for every live disco target. */

export type TargetAvailability = "always" | "configured" | "dynamic-entity";
export type ObservationPolicy =
  | "exact-when-available"
  | "runtime-extensible"
  | "runtime-dependent";

export interface TargetIdentity {
  category: string;
  type: string;
}

export interface TargetContractEntry {
  slug: string;
  availability: TargetAvailability;
  observationPolicy: ObservationPolicy;
  identities: TargetIdentity[];
  requiredFeatures: string[];
  independentlyOptionalFeatures: string[];
  runtimeFeatureVariants: string[][];
  claimableFeatures: string[];
}

export interface DiscoTargetContract {
  bySlug: Map<string, TargetContractEntry>;
  targets: TargetContractEntry[];
  sha256: string;
}

export function requireBoundedDiscoName(value: unknown, label: string): string {
  if (
    typeof value !== "string"
    || value.length === 0
    || value.length > 64
    || !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(value)
  ) {
    fail(label + " must be a bounded lowercase disco name");
  }
  return value;
}

export function requirePrivacySafeFeature(feature: string, label: string): void {
  if (feature.length > 256 || /[\s?&=@]/.test(feature)) {
    fail(label + " must be a bounded privacy-safe XMPP feature URI");
  }
  if (feature.startsWith("urn:waddle:")) {
    if (!/^urn:waddle:(?:[a-z][a-z0-9-]{0,31}:)+[0-9]{1,3}$/.test(feature)) {
      fail(
        label
          + " must use a versioned checked-in Waddle namespace without dynamic values",
      );
    }
    return;
  }
  if (
    /^(?:urn:(?:xmpp|ietf):|jabber:|storage:|vcard-temp(?::|$))/.test(feature)
    || /^(?:msgoffline|muc_[a-z_]+)$/.test(feature)
  ) return;
  try {
    const url = new URL(feature);
    const registeredJabberNamespace = ["http:", "https:"].includes(url.protocol)
      && ["jabber.org", "www.jabber.org"].includes(url.hostname);
    const pinnedIsrNamespace = url.protocol === "https:"
      && url.hostname === "xmpp.org"
      && url.pathname === "/extensions/isr/0"
      && !url.hash;
    if (
      (!registeredJabberNamespace && !pinnedIsrNamespace)
      || url.username
      || url.password
      || url.search
    ) fail(label + " must use an approved XMPP feature namespace");
  } catch {
    fail(label + " must use an approved XMPP feature namespace");
  }
}

export function isOfficialXmppFeature(feature: string): boolean {
  return /^(?:urn:(?:xmpp|ietf):|jabber:|storage:|vcard-temp(?::|$)|msgoffline$|muc_[a-z_]+$)/
    .test(feature)
    || /^https?:\/\/(?:www\.)?jabber\.org\//.test(feature)
    || feature === "https://xmpp.org/extensions/isr/0";
}

export function requireUniqueStrings(value: unknown, label: string): string[] {
  const entries = requireStringArray(value, label);
  if (new Set(entries).size !== entries.length) {
    fail(label + " must contain unique values");
  }
  return entries;
}

function parseContractIdentity(
  value: unknown,
  label: string,
): TargetIdentity {
  const identity = requireRecord(value, label);
  const keys = identity.name === undefined
    ? ["category", "type"]
    : ["category", "type", "name"];
  requireExactKeys(identity, keys, label);
  if (identity.name !== undefined) {
    const name = requireString(identity, "name", label);
    if (name.length > 64 || /[\r\n]/.test(name)) {
      fail(label + ".name must be a bounded static display name");
    }
  }
  return {
    category: requireBoundedDiscoName(identity.category, label + ".category"),
    type: requireBoundedDiscoName(identity.type, label + ".type"),
  };
}

export function validateDiscoTargetContractArtifact(
  repositoryRoot: string,
  value: unknown,
  artifact: CapabilityArtifact,
): DiscoTargetContract {
  const label = "disco-target-contract artifact";
  const sourcePath = resolveTrustedRepositoryFile(
    repositoryRoot,
    "server/disco-target-contract.json",
    "server",
    label + ".source",
  );
  if (readPinnedFile(sourcePath, label + ".source").sha256 !== artifact.sha256) {
    fail(label + " bytes must exactly match server/disco-target-contract.json");
  }
  const contract = requireRecord(value, label);
  requireExactKeys(contract, [
    "schema_version",
    "resolved_jid_retention",
    "observed_identity_name_retention",
    "targets",
  ], label);
  if (contract.schema_version !== 2) fail(label + ".schema_version must be 2");
  if (
    contract.resolved_jid_retention !== "forbidden"
    || contract.observed_identity_name_retention !== "forbidden"
  ) {
    fail(label + " must forbid resolved JID and observed identity-name retention");
  }
  if (!Array.isArray(contract.targets)) fail(label + ".targets must be an array");
  const targets = contract.targets.map((entry, index): TargetContractEntry => {
    const targetLabel = label + ".targets[" + index + "]";
    const target = requireRecord(entry, targetLabel);
    requireExactKeys(target, [
      "slug",
      "jid_template",
      "availability",
      "collection_input",
      "identities",
      "observation_policy",
      "required_features",
      "independently_optional_features",
      "runtime_feature_variants",
      "claimable_features",
    ], targetLabel);
    const slug = requireString(target, "slug", targetLabel);
    if (!kebabPattern.test(slug) || slug.length > 64) {
      fail(targetLabel + ".slug must be bounded kebab-case");
    }
    const jidTemplate = requireString(target, "jid_template", targetLabel);
    if (
      jidTemplate.length > 128
      || !/^[a-z0-9.{}_-]+$/.test(jidTemplate)
      || !jidTemplate.includes("{")
    ) fail(targetLabel + ".jid_template must be a bounded unresolved template");
    const availability = target.availability;
    if (!["always", "configured", "dynamic-entity"].includes(String(availability))) {
      fail(targetLabel + ".availability is invalid");
    }
    if (
      typeof target.collection_input !== "string"
      || !kebabPattern.test(target.collection_input)
    ) fail(targetLabel + ".collection_input must be kebab-case");
    const observationPolicy = target.observation_policy;
    if (![
      "exact-when-available",
      "runtime-extensible",
      "runtime-dependent",
    ].includes(String(observationPolicy))) {
      fail(targetLabel + ".observation_policy is invalid");
    }
    if (!Array.isArray(target.identities) || target.identities.length === 0) {
      fail(targetLabel + ".identities must be a non-empty array");
    }
    const identities = target.identities.map((identity, identityIndex) =>
      parseContractIdentity(identity, targetLabel + ".identities[" + identityIndex + "]")
    );
    if (new Set(identities.map(JSON.stringify)).size !== identities.length) {
      fail(targetLabel + ".identities must be unique");
    }
    const requiredFeatures = requireUniqueStrings(
      target.required_features,
      targetLabel + ".required_features",
    );
    requiredFeatures.forEach((feature, featureIndex) =>
      requirePrivacySafeFeature(
        feature,
        targetLabel + ".required_features[" + featureIndex + "]",
      )
    );
    const independentlyOptionalFeatures = requireUniqueStrings(
      target.independently_optional_features,
      targetLabel + ".independently_optional_features",
    );
    independentlyOptionalFeatures.forEach((feature, featureIndex) =>
      requirePrivacySafeFeature(
        feature,
        targetLabel + ".independently_optional_features[" + featureIndex + "]",
      )
    );
    if (!Array.isArray(target.runtime_feature_variants)) {
      fail(targetLabel + ".runtime_feature_variants must be an array");
    }
    const runtimeFeatureVariants = target.runtime_feature_variants.map(
      (variant, variantIndex) => {
        const features = requireUniqueStrings(
          variant,
          targetLabel + ".runtime_feature_variants[" + variantIndex + "]",
        );
        if (features.length === 0) {
          fail(targetLabel + ".runtime_feature_variants must not contain an empty variant");
        }
        features.forEach((feature, featureIndex) =>
          requirePrivacySafeFeature(
            feature,
            targetLabel + ".runtime_feature_variants[" + variantIndex + "]["
              + featureIndex + "]",
          )
        );
        return features;
      },
    );
    const variantKeys = runtimeFeatureVariants.map((variant) =>
      JSON.stringify([...variant].sort())
    );
    if (new Set(variantKeys).size !== variantKeys.length) {
      fail(targetLabel + ".runtime_feature_variants must be unique");
    }
    const claimableFeatures = requireUniqueStrings(
      target.claimable_features,
      targetLabel + ".claimable_features",
    );
    claimableFeatures.forEach((feature, featureIndex) =>
      requirePrivacySafeFeature(
        feature,
        targetLabel + ".claimable_features[" + featureIndex + "]",
      )
    );
    const sameFeatures = (left: string[], right: string[]) =>
      JSON.stringify([...left].sort()) === JSON.stringify([...right].sort());
    const optional = new Set(independentlyOptionalFeatures);
    if (
      requiredFeatures.length === 0
      || requiredFeatures.some((feature) => optional.has(feature))
      || runtimeFeatureVariants.some((variant) => variant.some((feature) => optional.has(feature)))
    ) fail(targetLabel + " optional features must be disjoint from required/runtime features");
    const variantUnion = [...new Set(runtimeFeatureVariants.flat())];
    const variantIntersection = runtimeFeatureVariants.length === 0
      ? []
      : runtimeFeatureVariants[0].filter((feature) =>
        runtimeFeatureVariants.every((variant) => variant.includes(feature))
      );
    const expectedClaimable = [
      ...(runtimeFeatureVariants.length === 0 ? requiredFeatures : variantUnion),
      ...independentlyOptionalFeatures,
    ];
    const policyIsValid = observationPolicy === "exact-when-available"
      ? runtimeFeatureVariants.length === 0
        && independentlyOptionalFeatures.length === 0
        && sameFeatures(requiredFeatures, claimableFeatures)
      : observationPolicy === "runtime-extensible"
      ? runtimeFeatureVariants.length === 0
      : runtimeFeatureVariants.length > 0
        && sameFeatures(requiredFeatures, variantIntersection);
    if (!policyIsValid || !sameFeatures(expectedClaimable, claimableFeatures)) {
      fail(targetLabel + " feature variants and claimable union violate observation policy");
    }
    return {
      slug,
      availability: availability as TargetAvailability,
      observationPolicy: observationPolicy as ObservationPolicy,
      identities,
      requiredFeatures,
      independentlyOptionalFeatures,
      runtimeFeatureVariants,
      claimableFeatures,
    };
  });
  const bySlug = new Map(targets.map((target) => [target.slug, target]));
  if (targets.length === 0 || bySlug.size !== targets.length) {
    fail(label + ".targets must contain a non-empty unique target set");
  }
  return {
    targets,
    bySlug,
    sha256: artifact.sha256,
  };
}
