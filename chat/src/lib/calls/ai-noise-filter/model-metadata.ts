import { NOISE_MODEL_IDS, type NoiseModelId } from "./model-id";

/**
 * Human-facing identity for a noise model: a capability *tier* (the
 * quality/CPU trade-off the user actually reasons about) paired with the
 * technical *name* (kept visible for the technically curious and for honest
 * telemetry/debugging). Centralised so the settings selector and the #911
 * verified-state row never drift apart on labels.
 */
export type NoiseModelMeta = {
  id: NoiseModelId;
  tier: "Light" | "Balanced" | "Maximum";
  name: string;
  /** Tier · name, e.g. "Light · RNNoise". */
  label: string;
  /** Short CPU hint shown beside the option. */
  costHint: string;
};

const META: Readonly<Record<NoiseModelId, Omit<NoiseModelMeta, "label">>> = {
  rnnoise: { id: "rnnoise", tier: "Light", name: "RNNoise", costHint: "low CPU" },
  dtln: { id: "dtln", tier: "Balanced", name: "DTLN", costHint: "more CPU" },
  deepfilternet: {
    id: "deepfilternet",
    tier: "Maximum",
    name: "DeepFilterNet",
    costHint: "highest CPU",
  },
};

/** Display metadata for a single model. */
export function noiseModelMeta(id: NoiseModelId): NoiseModelMeta {
  const base = META[id];
  return { ...base, label: `${base.tier} · ${base.name}` };
}

/** All models in canonical light-to-heavy order, for the selector. */
export function orderedNoiseModelMetas(): NoiseModelMeta[] {
  return NOISE_MODEL_IDS.map(noiseModelMeta);
}
