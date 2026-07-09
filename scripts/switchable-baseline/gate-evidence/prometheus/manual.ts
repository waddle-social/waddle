import {
  expectedFaroSignalIds,
  fail,
  requireExactKeys,
  requireRecord,
  requireSortedExactStrings,
} from "../common";
import {
  MANUAL_FARO_NOTE,
  PARTIAL_EVIDENCE_CONCLUSION,
} from "../../evidence";

export function validateManualFaroRequirement(
  value: unknown,
  conclusion: unknown,
  label: string,
): void {
  const manualFaro = requireRecord(
    value,
    label + ".manualFaro",
  );
  requireExactKeys(
    manualFaro,
    ["status", "signalIds", "note"],
    label + ".manualFaro",
  );
  if (manualFaro.status !== "required") {
    fail(label + ".manualFaro.status must be required");
  }
  requireSortedExactStrings(
    manualFaro.signalIds,
    expectedFaroSignalIds,
    label + ".manualFaro.signalIds",
  );
  if (manualFaro.note !== MANUAL_FARO_NOTE) {
    fail(label + ".manualFaro.note must use the fixed privacy-reviewed text");
  }
  if (conclusion !== PARTIAL_EVIDENCE_CONCLUSION) {
    fail(label + ".conclusion must use the fixed privacy-reviewed text");
	}
}
