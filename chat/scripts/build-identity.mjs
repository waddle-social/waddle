import { resolveCommitIdentity } from "./resolve-commit-sha.mjs";
import {
  BUILD_IDENTITY_SCHEMA_VERSION,
  disabledBuildIdentityMarker,
  FARO_SCOPE_ENVIRONMENT_NAMES,
  faroBuildIdentityMarker,
  parseFaroBuildIdentityScope,
  requireBoundedBuildIdentityLabel,
} from "../src/build-identity-contract.ts";

export { BUILD_IDENTITY_SCHEMA_VERSION } from "../src/build-identity-contract.ts";
export const BUILD_IDENTITY_FILE_NAME = "build-identity.json";

export function resolveBuildIdentity(options = {}) {
  const env = options.env ?? process.env;
  const commitIdentity = resolveCommitIdentity(options);
  const commitSha = commitIdentity.commitSha;
  const faroUrl = stringValue(env.PUBLIC_FARO_URL);
  const faroEnabled = faroUrl.length > 0;
  if (env.WADDLE_REQUIRE_FARO_BUILD === "true" && !faroEnabled) {
    throw new Error("production deploys require PUBLIC_FARO_URL");
  }
  if (faroEnabled) assertSafeFaroUrl(faroUrl);
  const application = stringValue(env.PUBLIC_FARO_APP_NAME) || "waddle-chat";
  requireBoundedBuildIdentityLabel(application, "PUBLIC_FARO_APP_NAME");
  const scope = faroEnabled ? resolveFaroScope(env, commitSha) : null;
  if (scope && scope.sourceId !== application) {
    throw new Error("PUBLIC_FARO_SOURCE_ID must match PUBLIC_FARO_APP_NAME");
  }
  const release = faroEnabled ? commitSha : null;
  const identity = {
    schemaVersion: BUILD_IDENTITY_SCHEMA_VERSION,
    marker: "",
    commitSha,
    source: commitIdentity.source,
    faro: {
      enabled: faroEnabled,
      application,
      release,
      scope,
    },
  };
  identity.marker = buildIdentityMarker(identity);
  return identity;
}

export function buildIdentityMarker(identity) {
  if (!identity.faro.enabled) {
    return disabledBuildIdentityMarker(identity.commitSha);
  }
  return faroBuildIdentityMarker(identity.commitSha, identity.faro.scope);
}

export function serializeBuildIdentity(identity) {
  return `${JSON.stringify(identity, null, 2)}\n`;
}

export function parseBuildIdentity(value, label = BUILD_IDENTITY_FILE_NAME) {
  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new Error(`${label} must be valid JSON`);
  }
  return parsed;
}

export function assertBuildIdentityMatches(actual, expected, label = BUILD_IDENTITY_FILE_NAME) {
  assertBuildIdentityShape(actual, label);
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${label} does not match the expected build identity`);
  }
  if (actual.marker !== buildIdentityMarker(actual)) {
    throw new Error(`${label}.marker does not match its identity fields`);
  }
}

function assertBuildIdentityShape(identity, label) {
  assertObjectKeys(identity, ["schemaVersion", "marker", "commitSha", "source", "faro"], label);
  if (identity.schemaVersion !== BUILD_IDENTITY_SCHEMA_VERSION) {
    throw new Error(`${label}.schemaVersion must be ${BUILD_IDENTITY_SCHEMA_VERSION}`);
  }
  if (identity.commitSha !== "unknown" && !/^[0-9a-f]{40}$/.test(identity.commitSha)) {
    throw new Error(`${label}.commitSha must be unknown or a full lowercase commit SHA`);
  }
  assertObjectKeys(identity.source, sourceKeys(identity.source), `${label}.source`);
  if (identity.source.commitSha !== identity.commitSha) {
    throw new Error(`${label}.source.commitSha must match commitSha`);
  }
  if (identity.source.kind === "git") {
    if (identity.commitSha === "unknown") {
      throw new Error(`${label}.source.kind git requires a full commit SHA`);
    }
  } else if (identity.source.kind !== "unknown" || identity.commitSha !== "unknown") {
    throw new Error(`${label}.source.kind is invalid`);
  }
  assertObjectKeys(identity.faro, ["enabled", "application", "release", "scope"], `${label}.faro`);
  if (typeof identity.faro.enabled !== "boolean") {
    throw new Error(`${label}.faro metadata is invalid`);
  }
  requireBoundedBuildIdentityLabel(identity.faro.application, `${label}.faro.application`);
  if (identity.faro.enabled) {
    parseFaroBuildIdentityScope(
      identity.faro.scope,
      identity.commitSha,
      `${label}.faro.scope`,
    );
    if (identity.faro.release !== identity.commitSha) {
      throw new Error(`${label}.faro release must match commitSha`);
    }
  } else if (identity.faro.release !== null || identity.faro.scope !== null) {
    throw new Error(`${label}.faro disabled identity must not contain release scope`);
  }
}

function sourceKeys(source) {
  if (!source || typeof source !== "object" || Array.isArray(source)) {
    throw new Error(`${BUILD_IDENTITY_FILE_NAME}.source must be an object`);
  }
  return ["kind", "commitSha"];
}

function assertObjectKeys(value, keys, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${label} must contain exactly ${expected.join(", ")}`);
  }
}

function resolveFaroScope(env, commitSha) {
  if (commitSha === "unknown") {
    throw new Error("Faro builds require a full commit SHA");
  }
  const scope = {};
  for (const [field, envName] of Object.entries(FARO_SCOPE_ENVIRONMENT_NAMES)) {
    const value = stringValue(env[envName]);
    if (!value) throw new Error(`${envName} is required when PUBLIC_FARO_URL is configured`);
    scope[field] = requireBoundedBuildIdentityLabel(value, envName);
  }
  return parseFaroBuildIdentityScope(
    { ...scope, release: commitSha },
    commitSha,
    "Faro build identity scope",
  );
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function assertSafeFaroUrl(value) {
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error("PUBLIC_FARO_URL must be a valid HTTPS URL");
  }
  if (
    url.protocol !== "https:"
    || url.username
    || url.password
    || url.search
    || url.hash
  ) {
    throw new Error("PUBLIC_FARO_URL must be HTTPS with no credentials, query, or fragment");
  }
}
