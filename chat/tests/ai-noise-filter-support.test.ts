import { describe, expect, test } from "bun:test";
import {
  anyNoiseModelAvailable,
  noiseModelSupport,
  type NoiseModelSupportEnv,
} from "../src/lib/calls/ai-noise-filter/support";

const env = (over: Partial<NoiseModelSupportEnv> = {}): NoiseModelSupportEnv => ({
  hasAudioWorklet: true,
  ...over,
});

describe("noiseModelSupport — which models this browser can run", () => {
  test("without AudioWorklet, the worklet models are unavailable with a reason", () => {
    const support = noiseModelSupport(env({ hasAudioWorklet: false }));
    expect(support.rnnoise.available).toBe(false);
    expect(support.dtln.available).toBe(false);
    if (!support.rnnoise.available) expect(support.rnnoise.reason).toMatch(/AudioWorklet/i);
  });

  test("RNNoise and DTLN are available whenever AudioWorklet is", () => {
    const support = noiseModelSupport(env());
    expect(support.rnnoise.available).toBe(true);
    expect(support.dtln.available).toBe(true);
  });

  test("DeepFilterNet is deferred (disabled slot) with a self-hosting reason", () => {
    // Its only ready-made package fetches its model from a dead third-party
    // CDN and is out-of-band besides, so it ships disabled until we vendor
    // compliant self-hosted assets.
    const support = noiseModelSupport(env());
    expect(support.deepfilternet.available).toBe(false);
    if (!support.deepfilternet.available) {
      expect(support.deepfilternet.reason).toMatch(/self-hosted|coming|pending/i);
    }
  });
});

describe("anyNoiseModelAvailable — gate the whole control", () => {
  test("true when at least one model can run", () => {
    expect(anyNoiseModelAvailable(noiseModelSupport(env()))).toBe(true);
  });

  test("false when nothing can run (no AudioWorklet)", () => {
    expect(anyNoiseModelAvailable(noiseModelSupport(env({ hasAudioWorklet: false })))).toBe(false);
  });
});
