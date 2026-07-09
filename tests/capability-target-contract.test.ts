import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { resolve } from "node:path";
import { validateDiscoTargetContractArtifact } from "../scripts/switchable-baseline/gate-evidence/capability-contract";
import type { CapabilityArtifact } from "../scripts/switchable-baseline/gate-evidence/common";

const repositoryRoot = resolve(import.meta.dir, "..");
const sourcePath = resolve(repositoryRoot, "server/disco-target-contract.json");
const source = await Bun.file(sourcePath).text();
const sha256 = createHash("sha256").update(source).digest("hex");
const artifact: CapabilityArtifact = {
  role: "disco-target-contract",
  path: "docs/evidence/gate-0/capability/disco-target-contract.json",
  sha256,
  release: {
    serverCommit: "a".repeat(40),
    webCommit: "b".repeat(40),
  },
  window: {
    start: "2026-07-10T09:00:00Z",
    end: "2026-07-10T10:00:00Z",
  },
};

describe("Gate 0 disco target contract", () => {
  test("uses slugs rather than array position as target identity", () => {
    const document = JSON.parse(source) as { targets: unknown[] };
    document.targets.reverse();

    const contract = validateDiscoTargetContractArtifact(
      repositoryRoot,
      document,
      artifact,
    );

    expect(contract.bySlug.size).toBe(document.targets.length);
    expect(new Set(contract.targets.map(({ slug }) => slug)).size).toBe(
      contract.targets.length,
    );
  });

  test("rejects duplicate target slugs", () => {
    const document = JSON.parse(source) as { targets: unknown[] };
    document.targets[1] = structuredClone(document.targets[0]);

    expect(() =>
      validateDiscoTargetContractArtifact(repositoryRoot, document, artifact)
    ).toThrow("must contain a non-empty unique target set");
  });
});
