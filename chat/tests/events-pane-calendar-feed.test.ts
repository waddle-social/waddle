import { describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";

function props(overrides: Record<string, unknown> = {}) {
  return {
    events: [],
    isLoading: false,
    isPosting: false,
    error: null,
    canPost: false,
    selfJid: null,
    communityJid: "community.example.com",
    serverBaseUrl: "https://server.example.com",
    sessionId: "session-1",
    findMaster: () => null,
    ...overrides,
  };
}

function calendarFeedButton(html: string): string {
  const marker = "Copy feed URL";
  const markerIndex = html.indexOf(marker);
  if (markerIndex === -1) return "";
  const start = html.lastIndexOf("<button", markerIndex);
  const end = html.indexOf("</button>", markerIndex);
  return html.slice(start, end + "</button>".length);
}

describe("EventsPane calendar feed control", () => {
  test("renders an enabled copy button when feed context is available", async () => {
    const html = await renderVueComponent(
      "../src/components/community/EventsPane.vue",
      props(),
      import.meta.url,
    );

    expect(html).toContain("Copy feed URL");
    expect(html).toContain('role="status"');
    expect(calendarFeedButton(html)).not.toMatch(/\sdisabled(?:[=>\s])/);
  });

  test("keeps copy available while events are loading", async () => {
    const html = await renderVueComponent(
      "../src/components/community/EventsPane.vue",
      props({ isLoading: true }),
      import.meta.url,
    );

    expect(html).toContain("Copy feed URL");
    expect(calendarFeedButton(html)).not.toMatch(/\sdisabled(?:[=>\s])/);
  });

  test("disables copy when feed context is missing", async () => {
    const html = await renderVueComponent(
      "../src/components/community/EventsPane.vue",
      props({ communityJid: null }),
      import.meta.url,
    );

    expect(html).toContain("Copy feed URL");
    expect(calendarFeedButton(html)).toMatch(/\sdisabled(?:[=>\s])/);
  });
});
