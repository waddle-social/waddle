import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderVueComponent } from "./helpers/render-vue-sfc";

describe("BotsSettingsSection", () => {
  test("renders the heading, the create form and the initial loading state", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/BotsSettingsSection.vue",
      { serverBaseUrl: "https://server.example.com", sessionId: "session-1" },
      import.meta.url,
    );

    expect(html).toContain("Bots");
    expect(html).toContain("New bot");
    expect(html).toContain('placeholder="standup"');
    expect(html).toContain('placeholder="Standup Bot"');
    expect(html).toContain("Create bot");
    expect(html).toContain("Loading bots…");
    // SSR never fetches, so no bot rows, tokens or secrets can render.
    expect(html).not.toContain("wbt_");
    expect(html).not.toContain("Mint token");
    expect(html).not.toContain("session-1");
  });

  test("is rendered by the settings page with the server base url and session id", () => {
    const source = readFileSync(
      new URL("../src/components/chat/UserSettingsPage.vue", import.meta.url),
      "utf8",
    );
    expect(source).toContain(
      '<BotsSettingsSection :server-base-url="serverBaseUrl" :session-id="session.session_id" />',
    );
  });
});
