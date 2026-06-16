import { describe, expect, test } from "bun:test";
import { BackupCodecPolicy } from "livekit-client";
import { videoCodecSupport } from "../src/lib/calls/video-codec/support";
import { videoPublishPlan } from "../src/lib/calls/video-codec/video-publish";

const capable = videoCodecSupport({
  encode: ["video/vp8", "video/vp9"],
  decode: ["video/vp8", "video/vp9"],
});
const incapable = videoCodecSupport({ encode: ["video/vp8"], decode: ["video/vp8"] });

describe("videoPublishPlan — screen-share on a VP9-capable device", () => {
  test("publishes VP9", () => {
    const plan = videoPublishPlan({ source: "screen", capability: capable });
    expect(plan.publish.videoCodec).toBe("vp9");
  });

  test("enables SVC and an explicit VP8 backup codec", () => {
    const plan = videoPublishPlan({ source: "screen", capability: capable });
    expect(plan.publish.scalabilityMode).toMatch(/^L\d+T\d+/);
    expect(plan.publish.backupCodec).toEqual({ codec: "vp8" });
  });

  test("keeps capable viewers on VP9 in a mixed room via multi-codec simulcast", () => {
    // 'everybody good, capable devices better': the presenter sends VP9 AND
    // the VP8 backup at once, so a VP8-only (iOS) viewer can't drag capable
    // viewers down to VP8 the way the default regression policy would.
    const plan = videoPublishPlan({ source: "screen", capability: capable });
    expect(plan.publish.backupCodecPolicy).toBe(BackupCodecPolicy.SIMULCAST);
  });

  test("raises the focal-stream bitrate ceiling (~5 Mbps top layer) at a 24-30 fps target", () => {
    const plan = videoPublishPlan({ source: "screen", capability: capable });
    expect(plan.publish.screenShareEncoding?.maxBitrate).toBe(5_000_000);
    const fps = plan.publish.screenShareEncoding?.maxFramerate ?? 0;
    expect(fps).toBeGreaterThanOrEqual(24);
    expect(fps).toBeLessThanOrEqual(30);
  });

  test("sheds frames before resolution under pressure (text stays readable)", () => {
    const plan = videoPublishPlan({ source: "screen", capability: capable });
    expect(plan.publish.degradationPreference).toBe("maintain-resolution");
  });

  test("captures at source resolution capped at ~1440p with contentHint detail", () => {
    const plan = videoPublishPlan({ source: "screen", capability: capable });
    expect(plan.capture?.contentHint).toBe("detail");
    expect(plan.capture?.resolution?.height).toBe(1440);
  });
});

describe("videoPublishPlan — screen-share on a VP9-incapable device (iOS)", () => {
  test("falls back to VP8 with no SVC and no backup codec", () => {
    const plan = videoPublishPlan({ source: "screen", capability: incapable });
    expect(plan.publish.videoCodec).toBe("vp8");
    expect(plan.publish.scalabilityMode).toBeUndefined();
    expect(plan.publish.backupCodec).toBeUndefined();
  });

  test("keeps the fallback first-class: same raised bitrate, framerate and capture cap", () => {
    const fallback = videoPublishPlan({ source: "screen", capability: incapable });
    const best = videoPublishPlan({ source: "screen", capability: capable });
    expect(fallback.publish.screenShareEncoding).toEqual(best.publish.screenShareEncoding);
    expect(fallback.publish.degradationPreference).toBe("maintain-resolution");
    expect(fallback.capture).toEqual(best.capture);
  });
});

describe("videoPublishPlan — camera preserves current behavior (rails only this slice)", () => {
  test("a talking head sheds resolution before framerate and gets no codec override", () => {
    const plan = videoPublishPlan({ source: "camera", capability: capable });
    expect(plan.publish.degradationPreference).toBe("maintain-framerate");
    expect(plan.publish.videoCodec).toBeUndefined();
    expect(plan.publish.scalabilityMode).toBeUndefined();
  });

  test("camera capture is governed by the room defaults, not this plan", () => {
    const plan = videoPublishPlan({ source: "camera", capability: capable });
    expect(plan.capture).toBeNull();
  });
});
