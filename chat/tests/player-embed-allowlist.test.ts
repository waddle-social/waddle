import { describe, expect, test } from "bun:test";
import { isAllowedPlayerEmbedOrigin } from "@/lib/xmpp/player-embed-allowlist";

describe("isAllowedPlayerEmbedOrigin", () => {
  test("accepts youtube-nocookie and vimeo player origins", () => {
    expect(isAllowedPlayerEmbedOrigin("https://www.youtube-nocookie.com/embed/429A_VugWW0")).toBe(true);
    expect(isAllowedPlayerEmbedOrigin("https://player.vimeo.com/video/12345")).toBe(true);
  });

  test("rejects non-allowlisted and non-https origins", () => {
    expect(isAllowedPlayerEmbedOrigin("https://evil.example.com/embed/x")).toBe(false);
    expect(isAllowedPlayerEmbedOrigin("https://www.youtube.com/embed/x")).toBe(false);
    expect(isAllowedPlayerEmbedOrigin("http://www.youtube-nocookie.com/embed/x")).toBe(false);
    expect(isAllowedPlayerEmbedOrigin("not a url")).toBe(false);
  });

  test("rejects embed URLs with userinfo", () => {
    expect(isAllowedPlayerEmbedOrigin("https://user@www.youtube-nocookie.com/embed/x")).toBe(false);
    expect(isAllowedPlayerEmbedOrigin("https://user:pass@player.vimeo.com/video/1")).toBe(false);
  });
});
