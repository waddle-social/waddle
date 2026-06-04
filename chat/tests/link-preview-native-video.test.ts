import { describe, expect, test } from "bun:test";
import { linkPreviewFromWasm } from "@/lib/xmpp/wasm-message-codecs";
import { isHlsMediaType } from "@/lib/xmpp/native-video";

describe("linkPreviewFromWasm native video", () => {
  test("maps an https progressive og:video to preview.video", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://rawkode.academy/watch/yoke",
      title: "Hands-on Yoke",
      video: { url: "https://content.rawkode.academy/v/clip.mp4", media_type: "video/mp4" },
    });
    expect(preview.video).toEqual({
      url: "https://content.rawkode.academy/v/clip.mp4",
      mediaType: "video/mp4",
    });
    expect(preview.playerEmbed).toBeUndefined();
  });

  test("accepts a media type carrying codec parameters", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://rawkode.academy/watch/yoke",
      video: { url: "https://content.rawkode.academy/v/clip.mp4", media_type: 'video/mp4; codecs="avc1.42E01E"' },
    });
    expect(preview.video).toEqual({
      url: "https://content.rawkode.academy/v/clip.mp4",
      mediaType: 'video/mp4; codecs="avc1.42E01E"',
    });
  });

  test("maps an HLS og:video to preview.video", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://rawkode.academy/watch/yoke",
      title: "Hands-on Yoke",
      video: { url: "https://content.rawkode.academy/v/stream.m3u8", media_type: "application/vnd.apple.mpegurl" },
    });
    expect(preview.video).toEqual({
      url: "https://content.rawkode.academy/v/stream.m3u8",
      mediaType: "application/vnd.apple.mpegurl",
    });
  });

  test("isHlsMediaType recognises HLS aliases, not progressive files", () => {
    expect(isHlsMediaType("application/vnd.apple.mpegurl")).toBe(true);
    expect(isHlsMediaType("application/x-mpegURL")).toBe(true);
    expect(isHlsMediaType("audio/x-mpegurl")).toBe(true);
    expect(isHlsMediaType("video/mp4")).toBe(false);
    expect(isHlsMediaType("video/webm")).toBe(false);
  });

  test("drops a non-https native video", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://example.com/watch",
      video: { url: "http://cdn.example.com/clip.mp4", media_type: "video/mp4" },
    });
    expect(preview.video).toBeUndefined();
  });

  test("drops an unsupported native video media type", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://example.com/watch",
      video: { url: "https://cdn.example.com/stream.flv", media_type: "video/x-flv" },
    });
    expect(preview.video).toBeUndefined();
  });

  test("drops a native video URL carrying userinfo", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://example.com/watch",
      video: { url: "https://user@cdn.example.com/clip.mp4", media_type: "video/mp4" },
    });
    expect(preview.video).toBeUndefined();
  });
});
