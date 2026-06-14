import { describe, expect, test } from "bun:test";
import {
  isNoiseModelId,
  modelIdFromProcessorName,
  NOISE_MODEL_IDS,
  processorName,
  type NoiseModelId,
} from "../src/lib/calls/ai-noise-filter/model-id";

describe("processorName — encodes a model id into a processor name", () => {
  test("namespaces the id under the waddle ai-noise-filter prefix", () => {
    expect(processorName("rnnoise")).toBe("waddle-ai-noise-filter:rnnoise");
    expect(processorName("dtln")).toBe("waddle-ai-noise-filter:dtln");
    expect(processorName("deepfilternet")).toBe("waddle-ai-noise-filter:deepfilternet");
  });
});

describe("modelIdFromProcessorName — recovers the model from a live processor's name", () => {
  test("round-trips every supported model id", () => {
    for (const id of NOISE_MODEL_IDS) {
      expect(modelIdFromProcessorName(processorName(id))).toBe(id);
    }
  });

  test("returns null for a foreign processor name (e.g. LiveKit's Krisp filter)", () => {
    // The honesty signal must not mistake some other processor for ours.
    expect(modelIdFromProcessorName("lk-krisp-noise-filter")).toBeNull();
  });

  test("returns null for our prefix with an unknown model suffix", () => {
    expect(modelIdFromProcessorName("waddle-ai-noise-filter:bogus")).toBeNull();
  });

  test("returns null when no processor is attached (undefined)", () => {
    expect(modelIdFromProcessorName(undefined)).toBeNull();
  });
});

describe("isNoiseModelId — narrows untrusted input (e.g. persisted prefs)", () => {
  test("accepts the three supported ids", () => {
    expect(isNoiseModelId("rnnoise")).toBe(true);
    expect(isNoiseModelId("dtln")).toBe(true);
    expect(isNoiseModelId("deepfilternet")).toBe(true);
  });

  test("rejects null, unknown strings, and non-strings", () => {
    expect(isNoiseModelId(null)).toBe(false);
    expect(isNoiseModelId("off")).toBe(false);
    expect(isNoiseModelId("krisp")).toBe(false);
    expect(isNoiseModelId(42)).toBe(false);
  });
});

describe("NOISE_MODEL_IDS — the canonical ordered set", () => {
  test("is exactly the three models, light-to-heavy", () => {
    const ids: readonly NoiseModelId[] = NOISE_MODEL_IDS;
    expect(ids).toEqual(["rnnoise", "dtln", "deepfilternet"]);
  });
});
