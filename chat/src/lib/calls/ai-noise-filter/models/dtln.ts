import { createNoiseSuppressionAudioWorklet } from "@workadventure/noise-suppression/audio-worklet";
import { processorName } from "../model-id";
import { WorkletNoiseProcessor } from "../processor";
import type { NoiseModelBackend } from "../registry";

/**
 * DTLN (MIT) via `@workadventure/noise-suppression` — DTLN models on the
 * LiteRT.js WASM runtime, bundled inside the worklet and served from our
 * origin by its Vite plugin (registered in `astro.config.mjs`). No remote
 * fetch. Forced single-threaded (`threads: false`) so it never reaches for
 * `SharedArrayBuffer` and needs no cross-origin isolation.
 */
type DtlnHandle = Awaited<ReturnType<typeof createNoiseSuppressionAudioWorklet>>;

class DtlnProcessor extends WorkletNoiseProcessor {
  readonly name = processorName("dtln");
  private handle?: DtlnHandle;

  protected async createWorkletNode(context: AudioContext): Promise<AudioWorkletNode> {
    const handle = await createNoiseSuppressionAudioWorklet(context, { threads: false });
    await handle.ready;
    this.handle = handle;
    return handle.node;
  }

  protected disposeWorkletNode(): void {
    this.handle?.dispose();
    this.handle = undefined;
  }
}

export const dtlnBackend: NoiseModelBackend = {
  id: "dtln",
  createProcessor: () => new DtlnProcessor(),
};
