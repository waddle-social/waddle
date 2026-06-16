import { describe, expect, test } from "bun:test";
import {
  videoCodecSupport,
  type VideoCodecSupportEnv,
} from "../src/lib/calls/video-codec/support";

const env = (over: Partial<VideoCodecSupportEnv> = {}): VideoCodecSupportEnv => ({
  encode: ["video/vp8", "video/vp9"],
  decode: ["video/vp8", "video/vp9"],
  ...over,
});

describe("videoCodecSupport — which codecs this device can publish", () => {
  test("VP9 is available when the device can both encode and decode it", () => {
    const support = videoCodecSupport(env());
    expect(support.vp9.available).toBe(true);
  });
});
