import { describe, expect, test } from "bun:test";
import { hasNoiseModelBackend } from "../src/lib/calls/ai-noise-filter/registry";

describe("noise model registry — which models ship a self-hosted backend", () => {
  test("RNNoise and DTLN have backends", () => {
    expect(hasNoiseModelBackend("rnnoise")).toBe(true);
    expect(hasNoiseModelBackend("dtln")).toBe(true);
  });

  test("DeepFilterNet has no backend (deferred slot)", () => {
    // It is shown disabled in the UI; selecting it is impossible, so the
    // registry deliberately carries no loader for it.
    expect(hasNoiseModelBackend("deepfilternet")).toBe(false);
  });
});
