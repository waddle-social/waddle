import { createHash } from "node:crypto";

import type { EvidenceRelease } from "../gate-evidence/common";
import type { ReleaseArtifactProvenance } from "./model";

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (typeof value === "object" && value !== null) {
    return `{${Object.entries(value)
      .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
      .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
      .join(",")}}`;
  }
  const scalar = JSON.stringify(value);
  if (scalar === undefined) {
    throw new Error("release artifact set contains a non-JSON value");
  }
  return scalar;
}

export function serverReleaseArtifactSetSha256(
  release: EvidenceRelease,
  artifacts: ReleaseArtifactProvenance["artifacts"],
): string {
  return createHash("sha256")
    .update(canonicalJson({
      schemaVersion: 1,
      kind: "waddle-server-release-artifact-set",
      serverCommit: release.serverCommit,
      artifacts: {
        serverImage: artifacts.serverImage,
        helmChart: artifacts.helmChart,
        gitOps: artifacts.gitOps,
        extensions: artifacts.extensions,
      },
    }))
    .digest("hex");
}

export function webReleaseArtifactSetSha256(
  release: EvidenceRelease,
  artifacts: ReleaseArtifactProvenance["artifacts"],
): string {
  return createHash("sha256")
    .update(canonicalJson({
      schemaVersion: 1,
      kind: "waddle-web-release-artifact-set",
      webCommit: release.webCommit,
      artifact: artifacts.web,
    }))
    .digest("hex");
}
