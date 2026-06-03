import { describe, expect, test } from "bun:test";
import { linkPreviewMediaState, type LinkPreview } from "@/lib/chat-ui";

describe("linkPreviewMediaState player kind", () => {
  test("returns player kind with poster when a playerEmbed is present", () => {
    const preview: LinkPreview = {
      originalUrl: "https://www.youtube.com/watch?v=429A_VugWW0",
      title: "A video",
      image: { url: "https://waddle.example/api/files/x.png", mediaType: "image/png" },
      playerEmbed: { url: "https://www.youtube-nocookie.com/embed/429A_VugWW0", width: 1280, height: 720 },
    };
    const state = linkPreviewMediaState(preview);
    expect(state.kind).toBe("player");
    if (state.kind === "player") {
      expect(state.player.url).toBe("https://www.youtube-nocookie.com/embed/429A_VugWW0");
      expect(state.poster?.url).toBe("https://waddle.example/api/files/x.png");
    }
  });

  test("prefers video over player when both somehow present", () => {
    const preview: LinkPreview = {
      originalUrl: "https://x.example/v",
      video: { url: "https://cdn.example/clip.mp4", mediaType: "video/mp4" },
      playerEmbed: { url: "https://player.vimeo.com/video/1" },
    };
    expect(linkPreviewMediaState(preview).kind).toBe("video");
  });
});
