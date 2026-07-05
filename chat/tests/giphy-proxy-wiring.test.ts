import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

function read(relative: string): string {
  return readFileSync(new URL(`../${relative}`, import.meta.url), "utf8");
}

describe("giphy proxy wiring", () => {
  test("the API route reads the key server-side and delegates to the pure handler", () => {
    const source = read("src/pages/api/giphy.ts");
    expect(source).toContain('from "cloudflare:workers"');
    expect(source).toContain("GIPHY_API_KEY");
    expect(source).toContain("handleGiphyProxyRequest");
  });

  test("GifPicker fetches the same-origin proxy and never talks to Giphy directly", () => {
    const source = read("src/components/chat/GifPicker.vue");
    expect(source).toContain("/api/giphy");
    expect(source).not.toContain("api.giphy.com");
    expect(source).not.toContain("api_key");
    expect(source).not.toContain("apiKey");
  });

  test("the giphyApiKey prop chain is gone", () => {
    const chain = [
      "src/layouts/AppLayout.astro",
      "src/components/AppShell.vue",
      "src/shell/chat-app-controller.ts",
      "src/components/chat/ChatReadyShell.vue",
      "src/components/chat/ContentArea.vue",
      "src/components/chat/ThreadPanel.vue",
      "src/components/calls/CallChatPanel.vue",
      "src/components/calls/CallExpandedSurface.vue",
      "src/components/chat/MessageComposer.vue",
    ];
    for (const relative of chain) {
      const source = read(relative);
      expect(source).not.toContain("giphyApiKey");
      expect(source).not.toContain("giphy-api-key");
      expect(source).not.toContain("GIPHY_API_KEY");
    }
  });
});
