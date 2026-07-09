import {
  assertGateZeroSignalSet,
  parseBaselineCatalog,
} from "../catalog";
import type {
  BaselineCatalog,
  BaselineSignal,
} from "../model";
import { fail, requireExactKeys, requireRecord, requireSha256, requireString } from "./common";
import {
  readTrustedJsonSnapshot,
} from "./filesystem";

export type CatalogSignal = BaselineSignal;

export type TrustedCatalog = BaselineCatalog & {
  schemaVersion: 1;
  sha256: string;
};

/**
 * Validate the evidence catalog reference, then use the production collector's
 * parser for the catalog itself. Keeping one schema/parser prevents evidence
 * acceptance from drifting away from what the collector can actually execute.
 */
export function parseCatalogReference(
  repositoryRoot: string,
  value: unknown,
  label: string,
): TrustedCatalog {
  const reference = requireRecord(value, label);
  requireExactKeys(reference, ["path", "schemaVersion", "sha256"], label);
  const path = requireString(reference, "path", label);
  if (path !== "docs/observability/switchable-baseline-signals.json") {
    fail(label + ".path must reference the switchable baseline signal catalog");
  }
  if (reference.schemaVersion !== 1) fail(label + ".schemaVersion must be 1");
  const expectedSha256 = requireSha256(reference.sha256, label + ".sha256");
  const catalogSnapshot = readTrustedJsonSnapshot(
    repositoryRoot,
    path,
    "docs/observability",
    label + ".path",
    ".json",
    expectedSha256,
  );
  if (catalogSnapshot.sha256 !== expectedSha256) {
    fail(label + ".sha256 does not match the catalog bytes");
  }

  let catalog: BaselineCatalog;
  try {
    catalog = parseBaselineCatalog(catalogSnapshot.value);
  } catch (error) {
    fail(error instanceof Error ? error.message : "signal catalog is invalid");
  }
  if (catalog.schemaVersion !== 1 || catalog.milestone !== "switchable-alternative") {
    fail("signal catalog must use schema 1 for the switchable-alternative milestone");
  }
  if (catalog.minimumCollectionWindowMinutes < 60) {
    fail("signal catalog minimum collection window must be at least 60 minutes");
  }
  try {
    assertGateZeroSignalSet(catalog);
  } catch (error) {
    fail(error instanceof Error ? error.message : "signal catalog set is invalid");
  }
  return Object.assign(catalog, { schemaVersion: 1 as const, sha256: expectedSha256 });
}
