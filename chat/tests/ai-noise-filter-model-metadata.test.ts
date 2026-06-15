import { describe, expect, test } from "bun:test";
import {
  noiseModelMeta,
  orderedNoiseModelMetas,
} from "../src/lib/calls/ai-noise-filter/model-metadata";

describe("noiseModelMeta — human-facing identity per model", () => {
  test("pairs a capability tier with the technical name", () => {
    expect(noiseModelMeta("rnnoise")).toMatchObject({
      id: "rnnoise",
      tier: "Light",
      name: "RNNoise",
      label: "Light · RNNoise",
    });
    expect(noiseModelMeta("dtln")).toMatchObject({
      tier: "Balanced",
      name: "DTLN",
      label: "Balanced · DTLN",
    });
    expect(noiseModelMeta("deepfilternet")).toMatchObject({
      tier: "Maximum",
      name: "DeepFilterNet",
      label: "Maximum · DeepFilterNet",
    });
  });

  test("every model carries a short cost hint for the selector", () => {
    expect(noiseModelMeta("rnnoise").costHint.length).toBeGreaterThan(0);
    expect(noiseModelMeta("deepfilternet").costHint.length).toBeGreaterThan(0);
  });
});

describe("orderedNoiseModelMetas — selector render order, light-to-heavy", () => {
  test("lists the three models in canonical order", () => {
    expect(orderedNoiseModelMetas().map((m) => m.id)).toEqual([
      "rnnoise",
      "dtln",
      "deepfilternet",
    ]);
  });
});
