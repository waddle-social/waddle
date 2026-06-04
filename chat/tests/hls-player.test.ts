import { describe, expect, test } from "bun:test";
import { videoPlaybackStrategy } from "@/lib/xmpp/hls-player";

describe("videoPlaybackStrategy", () => {
  test("HLS without native support uses hls.js", () => {
    expect(videoPlaybackStrategy("application/vnd.apple.mpegurl", false)).toBe("hls-js");
    expect(videoPlaybackStrategy("application/x-mpegURL", false)).toBe("hls-js");
  });

  test("HLS with native support (Safari) uses the native <video> src", () => {
    expect(videoPlaybackStrategy("application/vnd.apple.mpegurl", true)).toBe("native-src");
  });

  test("progressive media always uses the native <video> src", () => {
    expect(videoPlaybackStrategy("video/mp4", false)).toBe("native-src");
    expect(videoPlaybackStrategy("video/webm", true)).toBe("native-src");
  });
});
