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

  test("unavailable with a WebGL2 reason when segmentation compositing is missing", () => {
    const support = backgroundEffectSupport({
      hasSegmentationCompositing: false,
      hasFramePipeline: true,
    });

    expect(support.available).toBe(false);
    // The reason is user-facing (drives the disabled-control copy); pin its
    // gist so a refactor that blanks it is caught.
    if (!support.available) expect(support.reason).toMatch(/WebGL2/i);
  });

  test("unavailable with a frame-pipeline reason when no pipeline exists", () => {
    const support = backgroundEffectSupport({
      hasSegmentationCompositing: true,
      hasFramePipeline: false,
    });

    expect(support.available).toBe(false);
    if (!support.available) expect(support.reason).toMatch(/frames|background/i);
  });
});
