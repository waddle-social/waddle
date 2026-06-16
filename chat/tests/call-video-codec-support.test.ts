import { describe, expect, test } from "bun:test";
import {
  currentVideoCodecSupportEnv,
  videoCodecSupport,
  type VideoCodecSupportEnv,
} from "../src/lib/calls/video-codec/support";

const capabilitiesOf = (mimeTypes: string[]) => ({
  getCapabilities: () => ({ codecs: mimeTypes.map((mimeType) => ({ mimeType })) }),
});

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

  test("VP9 is unavailable, with a reason, when the device can't encode it (iOS)", () => {
    const support = videoCodecSupport(env({ encode: ["video/vp8"] }));
    expect(support.vp9.available).toBe(false);
    if (!support.vp9.available) expect(support.vp9.reason).toMatch(/encode.*vp9/i);
  });

  test("VP9 is unavailable when the device can encode but not decode it (no peer playback)", () => {
    const support = videoCodecSupport(env({ decode: ["video/vp8"] }));
    expect(support.vp9.available).toBe(false);
    if (!support.vp9.available) expect(support.vp9.reason).toMatch(/decode.*vp9/i);
  });

  test("codec mimeType matching is case-insensitive (browsers report 'video/VP9')", () => {
    const support = videoCodecSupport(env({ encode: ["video/VP8", "video/VP9"], decode: ["video/VP8", "video/VP9"] }));
    expect(support.vp9.available).toBe(true);
    expect(support.vp8.available).toBe(true);
  });
});

describe("currentVideoCodecSupportEnv — read real sender/receiver capabilities", () => {
  test("maps injected sender/receiver capabilities into encode/decode mimeTypes", () => {
    const env = currentVideoCodecSupportEnv({
      sender: capabilitiesOf(["video/VP8", "video/VP9"]),
      receiver: capabilitiesOf(["video/VP8"]),
    });
    expect(env.encode).toEqual(["video/VP8", "video/VP9"]);
    expect(env.decode).toEqual(["video/VP8"]);
  });

  test("a platform without getCapabilities reports no codecs (everything unavailable)", () => {
    const env = currentVideoCodecSupportEnv({ sender: {}, receiver: {} });
    expect(videoCodecSupport(env).vp9.available).toBe(false);
    expect(videoCodecSupport(env).vp8.available).toBe(false);
  });
});
