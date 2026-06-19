import { describe, expect, mock, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import {
  CanvasVirtualBackgroundProcessor,
  virtualBackgroundAlphaFromConfidence,
} from "../src/lib/calls/virtual-background/processor";

describe("virtual background confidence mask", () => {
  test("feathers uncertain foreground pixels instead of hard-thresholding them", () => {
    expect(
      Array.from(virtualBackgroundAlphaFromConfidence([0, 0.35, 0.55, 0.75, 1])),
    ).toEqual([0, 0, 128, 255, 255]);
  });

  test("smooths alpha changes against the previous frame to reduce edge flicker", () => {
    const previousAlpha = new Uint8ClampedArray([128]);

    expect(
      Array.from(virtualBackgroundAlphaFromConfidence([0.75], previousAlpha)),
    ).toEqual([179]);
  });
});

describe("CanvasVirtualBackgroundProcessor lifecycle", () => {
  test("keeps the output canvas alpha-capable for foreground compositing", async () => {
    const env = installVirtualBackgroundDom();
    const processor = new CanvasVirtualBackgroundProcessor(
      { kind: "blur" },
      {
        loadSegmenter: () =>
          Promise.resolve({
            segmentForVideo: () => ({
              confidenceMasks: [
                {
                  width: 1,
                  height: 1,
                  getAsFloat32Array: () => new Float32Array([1]),
                },
              ],
              close: mock(() => undefined),
            }),
          } as never),
        setFrameTimer: mock((() => 42) as unknown as typeof setInterval),
        clearFrameTimer: mock((() => undefined) as unknown as typeof clearInterval),
      },
    );

    try {
      await processor.init({ track: env.inputTrack as MediaStreamTrack });
      expect(env.canvases[0]?.contextOptions).not.toEqual({ alpha: false });
    } finally {
      await processor.destroy();
      env.restore();
    }
  });

  test("feathers the mask draw before foreground compositing", async () => {
    const env = installVirtualBackgroundDom();
    const processor = new CanvasVirtualBackgroundProcessor(
      { kind: "blur" },
      {
        loadSegmenter: () =>
          Promise.resolve({
            segmentForVideo: () => ({
              confidenceMasks: [
                {
                  width: 1,
                  height: 1,
                  getAsFloat32Array: () => new Float32Array([1]),
                },
              ],
              close: mock(() => undefined),
            }),
          } as never),
        setFrameTimer: mock((() => 42) as unknown as typeof setInterval),
        clearFrameTimer: mock((() => undefined) as unknown as typeof clearInterval),
      },
    );

    try {
      await processor.init({ track: env.inputTrack as MediaStreamTrack });
      expect(env.canvases[0]?.context.drawFilters).toContain("blur(3px)");
    } finally {
      await processor.destroy();
      env.restore();
    }
  });

  test("tears down the partial graph when the first segmentation pass fails", async () => {
    const env = installVirtualBackgroundDom();
    const setFrameTimer = mock((() => 42) as unknown as typeof setInterval);
    const clearFrameTimer = mock((() => undefined) as unknown as typeof clearInterval);
    const failure = new Error("missing mediapipe assets");
    const processor = new CanvasVirtualBackgroundProcessor(
      { kind: "blur" },
      {
        loadSegmenter: () => Promise.reject(failure),
        setFrameTimer,
        clearFrameTimer,
      },
    );

    try {
      await expect(
        processor.init({ track: env.inputTrack as MediaStreamTrack }),
      ).rejects.toThrow("missing mediapipe assets");
    } finally {
      env.restore();
    }

    expect(setFrameTimer).not.toHaveBeenCalled();
    expect(clearFrameTimer).not.toHaveBeenCalled();
    expect(env.video.pause).toHaveBeenCalledTimes(1);
    expect(env.video.srcObject).toBeNull();
    expect(env.outputTrack.stop).toHaveBeenCalledTimes(1);
    expect(processor.processedTrack).toBeUndefined();
  });
});

describe("self-hosted MediaPipe assets", () => {
  test("ships every WASM binary referenced by the local loader scripts", () => {
    const wasmDir = join(import.meta.dir, "../public/mediapipe/wasm");
    const loaders = [
      "vision_wasm_internal.js",
      "vision_wasm_module_internal.js",
      "vision_wasm_nosimd_internal.js",
    ];

    for (const loader of loaders) {
      const loaderPath = join(wasmDir, loader);
      const contents = readFileSync(loaderPath, "utf8");
      const referencedWasm = new Set(
        Array.from(contents.matchAll(/["`]([^"`]+\.wasm)["`]/g), ([, path]) =>
          basename(path ?? ""),
        ).filter(Boolean),
      );

      for (const wasm of referencedWasm) {
        expect(
          existsSync(join(dirname(loaderPath), wasm)),
          `${loader} references missing ${wasm}`,
        ).toBe(true);
      }
    }
  });
});

function installVirtualBackgroundDom() {
  const globals = globalThis as unknown as Record<string, unknown>;
  const previous = {
    document: globals.document,
    MediaStream: globals.MediaStream,
    HTMLMediaElement: globals.HTMLMediaElement,
    ImageData: globals.ImageData,
    performance: globals.performance,
  };

  const inputTrack = { id: "input-camera" };
  const outputTrack = { id: "processed-camera", stop: mock(() => undefined) };
  const video = new FakeVideoElement();
  const canvases: FakeCanvasElement[] = [];

  globals.HTMLMediaElement = class {
    static HAVE_CURRENT_DATA = 2;
  };
  globals.ImageData = class {
    readonly data: Uint8ClampedArray;
    constructor(
      readonly width: number,
      readonly height: number,
    ) {
      this.data = new Uint8ClampedArray(width * height * 4);
    }
  };
  globals.MediaStream = class {
    constructor(private readonly tracks: unknown[] = []) {}
    getVideoTracks(): unknown[] {
      return this.tracks;
    }
    getTracks(): unknown[] {
      return this.tracks;
    }
  };
  globals.performance = { now: () => 123 };
  globals.document = {
    createElement(tag: string) {
      if (tag === "video") return video;
      if (tag === "canvas") {
        const canvas = new FakeCanvasElement(outputTrack);
        canvases.push(canvas);
        return canvas;
      }
      throw new Error(`Unexpected element: ${tag}`);
    },
  };

  return {
    inputTrack,
    outputTrack,
    video,
    canvases,
    restore() {
      globals.document = previous.document;
      globals.MediaStream = previous.MediaStream;
      globals.HTMLMediaElement = previous.HTMLMediaElement;
      globals.ImageData = previous.ImageData;
      globals.performance = previous.performance;
    },
  };
}

class FakeVideoElement {
  muted = false;
  playsInline = false;
  autoplay = false;
  srcObject: unknown = null;
  videoWidth = 640;
  videoHeight = 360;
  readyState = 2;
  readonly play = mock(() => Promise.resolve());
  readonly pause = mock(() => undefined);
  addEventListener(): void {}
  removeEventListener(): void {}
}

class FakeCanvasElement {
  width = 0;
  height = 0;
  contextOptions: CanvasRenderingContext2DSettings | undefined;
  readonly context = new FakeCanvasContext();

  constructor(private readonly outputTrack: unknown) {}

  getContext(_contextId?: string, options?: CanvasRenderingContext2DSettings): FakeCanvasContext {
    this.contextOptions = options;
    return this.context;
  }

  captureStream(): MediaStream {
    return new MediaStream([this.outputTrack as MediaStreamTrack]);
  }
}

class FakeCanvasContext {
  filter = "none";
  globalCompositeOperation = "source-over";
  readonly drawFilters: string[] = [];
  save(): void {}
  clearRect(): void {}
  drawImage(): void {
    this.drawFilters.push(this.filter);
  }
  restore(): void {}
  putImageData(): void {}
}
