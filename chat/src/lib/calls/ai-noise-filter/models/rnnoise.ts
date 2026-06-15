import { loadRnnoise, RnnoiseWorkletNode } from "@sapphi-red/web-noise-suppressor";
import rnnoiseWorkletUrl from "@sapphi-red/web-noise-suppressor/rnnoiseWorklet.js?url";
import rnnoiseWasmUrl from "@sapphi-red/web-noise-suppressor/rnnoise.wasm?url";
import rnnoiseSimdWasmUrl from "@sapphi-red/web-noise-suppressor/rnnoise_simd.wasm?url";
import { processorName } from "../model-id";
import { WorkletNoiseProcessor } from "../processor";
import type { NoiseModelBackend } from "../registry";

/**
 * RNNoise (MIT) via `@sapphi-red/web-noise-suppressor`. The worklet `.js` and
 * `.wasm` are imported with Vite's `?url` so they are emitted into our own
 * Cloudflare Pages build and served from our origin — self-hosted, no CDN,
 * no out-of-band fetch. Single-threaded; no cross-origin isolation needed.
 */

// Memoize the wasm binary at module scope so a mid-call device switch or a
// re-enable re-uses it instead of refetching. On rejection (e.g. a transient
// network hiccup on the first selection) the cache is cleared so an explicit
// re-selection can retry — otherwise a rejected promise would stick forever.
let wasmBinary: Promise<ArrayBuffer> | undefined;
function loadWasm(): Promise<ArrayBuffer> {
  if (!wasmBinary) {
    wasmBinary = loadRnnoise({ url: rnnoiseWasmUrl, simdUrl: rnnoiseSimdWasmUrl });
    wasmBinary.catch(() => {
      wasmBinary = undefined;
    });
  }
  return wasmBinary;
}

class RnnoiseProcessor extends WorkletNoiseProcessor {
  readonly name = processorName("rnnoise");

  protected async createWorkletNode(context: AudioContext): Promise<AudioWorkletNode> {
    const binary = await loadWasm();
    await context.audioWorklet.addModule(rnnoiseWorkletUrl);
    return new RnnoiseWorkletNode(context, { wasmBinary: binary, maxChannels: 1 });
  }

  protected disposeWorkletNode(node: AudioWorkletNode): void {
    (node as RnnoiseWorkletNode).destroy();
  }
}

export const rnnoiseBackend: NoiseModelBackend = {
  id: "rnnoise",
  createProcessor: () => new RnnoiseProcessor(),
};
