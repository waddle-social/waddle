import { describe, expect, test } from "bun:test";
import { linkPreviewFromWasm } from "@/lib/xmpp/wasm-message-codecs";

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
