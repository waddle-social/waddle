import { describe, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import {
  liveKitBackgroundProcessorSwitchOptionsForEffect,
  liveKitBackgroundProcessorOptionsForEffect,
  virtualBackgroundProcessorName,
} from "../src/lib/calls/virtual-background/processor";
import { makeVirtualBackgroundProcessor } from "../src/lib/calls/virtual-background/registry";

describe("virtual background LiveKit processor adapter", () => {
  test("maps blur to LiveKit's background-blur mode with self-hosted assets", () => {
    expect(liveKitBackgroundProcessorOptionsForEffect({ kind: "blur" })).toEqual({
      mode: "background-blur",
      blurRadius: 16,
      maxFps: 24,
      assetPaths: {
        tasksVisionFileSet: "/mediapipe/tasks-vision/wasm",
        modelAssetPath: "/mediapipe/models/selfie_segmenter_landscape.tflite",
      },
    });
  });

  test("maps image replacement to LiveKit's virtual-background mode without persisting bytes in the processor name", () => {
    const imageUrl = "data:image/png;base64,ZmFrZS1pbWFnZQ==";

    expect(liveKitBackgroundProcessorOptionsForEffect({ kind: "image", imageUrl })).toEqual({
      mode: "virtual-background",
      imagePath: imageUrl,
      maxFps: 24,
      assetPaths: {
        tasksVisionFileSet: "/mediapipe/tasks-vision/wasm",
        modelAssetPath: "/mediapipe/models/selfie_segmenter_landscape.tflite",
      },
    });
    expect(virtualBackgroundProcessorName({ kind: "image", imageUrl })).toBe(
      "waddle:virtual-background:image",
    );
  });

  test("maps off to LiveKit's disabled switch mode", () => {
    expect(liveKitBackgroundProcessorSwitchOptionsForEffect({ kind: "off" })).toEqual({
      mode: "disabled",
    });
  });

  test("rejects processor creation when LiveKit background processors are unsupported", async () => {
    await expect(
      makeVirtualBackgroundProcessor({ kind: "blur" }, () => false),
    ).rejects.toThrow("Virtual background processors are not supported in this browser");
  });

  test("ships the local LiveKit background processor assets it references", () => {
    const publicDir = join(import.meta.dir, "../public");
    const options = liveKitBackgroundProcessorOptionsForEffect({ kind: "blur" });
    const wasmDir = join(publicDir, options.assetPaths.tasksVisionFileSet);
    const loaders = [
      "vision_wasm_internal.js",
      "vision_wasm_module_internal.js",
      "vision_wasm_nosimd_internal.js",
    ];

    expect(existsSync(join(publicDir, options.assetPaths.modelAssetPath))).toBe(true);
    for (const loader of loaders) {
      const loaderPath = join(wasmDir, loader);
      expect(existsSync(loaderPath), `${loader} is missing`).toBe(true);
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
