import {
  fail,
  parseWindow,
  requireExactKeys,
  requireInteger,
  requireRecord,
  sameWindow,
  type EvidenceWindow,
} from "../common";
import type { TrustedCatalog } from "../catalog";
import { QUERY_STEP_SECONDS } from "../../model";

export function validateCollectionWindow(
  value: unknown,
  artifactWindow: EvidenceWindow,
  catalog: TrustedCatalog,
  label: string,
): { parsedWindow: EvidenceWindow; stepSeconds: number } {
  const collectionWindow = requireRecord(
    value,
    label + ".collectionWindow",
  );
  requireExactKeys(collectionWindow, [
    "start",
    "end",
    "durationMinutes",
    "minimumDurationMinutes",
    "stepSeconds",
  ], label + ".collectionWindow");
  const parsedWindow = parseWindow(
    { start: collectionWindow.start, end: collectionWindow.end },
    label + ".collectionWindow",
  );
  if (!sameWindow(parsedWindow, artifactWindow)) {
    fail(label + ".collectionWindow must match its artifact manifest entry");
  }
  const durationMinutes =
    (Date.parse(parsedWindow.end) - Date.parse(parsedWindow.start)) / 60_000;
  if (collectionWindow.durationMinutes !== durationMinutes) {
    fail(
      label
        + ".collectionWindow.durationMinutes must match the fixed window",
    );
  }
  if (
    collectionWindow.minimumDurationMinutes
    !== catalog.minimumCollectionWindowMinutes
  ) {
    fail(
      label
        + ".collectionWindow.minimumDurationMinutes must match the catalog",
    );
  }
  if (durationMinutes < catalog.minimumCollectionWindowMinutes) {
    fail(label + ".collectionWindow must meet the catalog minimum duration");
  }
  const stepSeconds = requireInteger(
    collectionWindow.stepSeconds,
    label + ".collectionWindow.stepSeconds",
    1,
  );
  if (stepSeconds !== QUERY_STEP_SECONDS) {
    fail(label + ".collectionWindow.stepSeconds must be exactly " + QUERY_STEP_SECONDS);
  }
  const durationSeconds = Date.parse(parsedWindow.end) / 1_000
    - Date.parse(parsedWindow.start) / 1_000;
  const startSeconds = Date.parse(parsedWindow.start) / 1_000;
  const endSeconds = Date.parse(parsedWindow.end) / 1_000;
  if (
    !Number.isInteger(durationMinutes)
    || durationSeconds % stepSeconds !== 0
    || startSeconds % stepSeconds !== 0
    || endSeconds % stepSeconds !== 0
  ) {
    fail(label + ".collectionWindow must align exactly to the fixed timestamp grid");
  }
  return { parsedWindow, stepSeconds };
}
