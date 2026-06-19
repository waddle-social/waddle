import { describe, expect, test } from "bun:test";
import { backgroundEffectSupport } from "../src/lib/calls/background-effect/support";

describe("backgroundEffectSupport", () => {
  test("available when the browser has both segmentation compositing and a frame pipeline", () => {
    const support = backgroundEffectSupport({
      hasSegmentationCompositing: true,
      hasFramePipeline: true,
    });

    expect(support).toEqual({ available: true });
  });

  test("unavailable with a reason when segmentation compositing is missing", () => {
    const support = backgroundEffectSupport({
      hasSegmentationCompositing: false,
      hasFramePipeline: true,
    });

    expect(support.available).toBe(false);
    expect(support).toHaveProperty("reason");
  });

  test("unavailable with a reason when no frame pipeline exists", () => {
    const support = backgroundEffectSupport({
      hasSegmentationCompositing: true,
      hasFramePipeline: false,
    });

    expect(support.available).toBe(false);
    expect(support).toHaveProperty("reason");
  });
});
