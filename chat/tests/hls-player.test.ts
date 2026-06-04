import { describe, expect, test } from "bun:test";
import { videoPlaybackStrategy } from "@/lib/xmpp/hls-player";

describe("videoPlaybackStrategy", () => {
  test("every HLS alias without native support uses hls.js", () => {
    for (const alias of [
      "application/vnd.apple.mpegurl",
      "application/x-mpegURL",
      "application/x-mpegurl",
      "audio/x-mpegurl",
      "audio/mpegurl",
      "application/mpegurl",
    ]) {
      expect(videoPlaybackStrategy(alias, false)).toBe("hls-js");
    }
  });

  test("HLS with native support (Safari) uses the native <video> src", () => {
    expect(videoPlaybackStrategy("application/vnd.apple.mpegurl", true)).toBe("native-src");
  });

  test("progressive media always uses the native <video> src", () => {
    expect(videoPlaybackStrategy("video/mp4", false)).toBe("native-src");
    expect(videoPlaybackStrategy("video/webm", true)).toBe("native-src");
  });
});
