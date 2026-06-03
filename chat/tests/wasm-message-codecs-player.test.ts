import { describe, expect, test } from "bun:test";
import { linkPreviewFromWasm } from "@/lib/xmpp/wasm-message-codecs";

describe("linkPreviewFromWasm player embed", () => {
  test("maps an allowlisted player embed", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://www.youtube.com/watch?v=429A_VugWW0",
      player_embed: { url: "https://www.youtube-nocookie.com/embed/429A_VugWW0", width: 1280, height: 720 },
    });
    expect(preview.playerEmbed?.url).toBe("https://www.youtube-nocookie.com/embed/429A_VugWW0");
    expect(preview.playerEmbed?.width).toBe(1280);
  });

  test("drops a non-allowlisted player embed", () => {
    const preview = linkPreviewFromWasm({
      original_url: "https://example.com/x",
      player_embed: { url: "https://evil.example.com/embed/x" },
    });
    expect(preview.playerEmbed).toBeUndefined();
  });
});
