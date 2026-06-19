import { describe, expect, test } from "bun:test";
import { computed, ref } from "vue";
import { renderVueComponent, renderVueComponentSource } from "./helpers/render-vue-sfc";
import {
  calendarFeedCopyControllerKey,
  type CalendarFeedCopyController,
} from "../src/lib/use-calendar-feed-copy";

const chatReadyShellSource = await Bun.file(
  new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url),
).text();

const computedCommunityJidBridge = `
<script setup lang="ts">
import { computed } from "vue";
import EventsPane from "@/components/community/EventsPane.vue";

const communityJid = computed(() => "community.example.com");
function findMaster() {
  return null;
}
</script>

<template>
  <EventsPane
    :events="[]"
    :is-loading="false"
    :is-posting="false"
    :error="null"
    :can-post="true"
    self-jid="alice@example.com"
    :community-jid="communityJid"
    server-base-url=""
    session-id="session-1"
    :find-master="findMaster"
  />
</template>
`;

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

function elementWithText(html: string, tag: string, text: string): string {
  const markerIndex = html.indexOf(text);
  if (markerIndex === -1) return "";
  const start = html.lastIndexOf(`<${tag}`, markerIndex);
  const end = html.indexOf(`</${tag}>`, markerIndex);
  return html.slice(start, end + tag.length + 3);
}

function elementWithAttribute(html: string, tag: string, attribute: string): string {
  const markerIndex = html.indexOf(attribute);
  if (markerIndex === -1) return "";
  const start = html.lastIndexOf(`<${tag}`, markerIndex);
  const end = html.indexOf(">", markerIndex);
  return html.slice(start, end + 1);
}

describe("EventsPane calendar feed control", () => {
  test("receives the shell's computed community JID instead of its nested value", () => {
    expect(chatReadyShellSource).toContain(':community-jid="communityJid"');
    expect(chatReadyShellSource).not.toContain(':community-jid="communityJid.value"');
  });

  test("enables copy when a parent passes a computed community JID", async () => {
    const html = await renderVueComponentSource(computedCommunityJidBridge);

    expect(html).toContain("Copy feed URL");
    expect(calendarFeedButton(html)).not.toMatch(/\sdisabled(?:[=>\s])/);
  });

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

  test("keeps copy available when the feed helper uses same-origin API routes", async () => {
    const html = await renderVueComponent(
      "../src/components/community/EventsPane.vue",
      props({ serverBaseUrl: "" }),
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

  test("renders a feed URL panel as both an open link and selectable value", async () => {
    const feedUrl = "https://xmpp.waddle.social/api/calendar/community/v1.community.signature/events.ics";
    const subscriptionHref = "webcal://xmpp.waddle.social/api/calendar/community/v1.community.signature/events.ics";
    const html = await renderVueComponent(
      "../src/components/community/CalendarFeedUrlPanel.vue",
      { url: feedUrl },
      import.meta.url,
    );
    const openLink = elementWithText(html, "a", "Subscribe to calendar");
    const urlInput = elementWithAttribute(html, "input", 'aria-label="Calendar feed URL"');

    expect(openLink).toContain(`href="${subscriptionHref}"`);
    expect(openLink).toContain('target="_blank"');
    expect(openLink).toContain('rel="noopener noreferrer"');
    expect(openLink).toContain("min-h-11");
    expect(openLink).not.toContain("sm:min-h-8");
    expect(urlInput).toContain(`value="${feedUrl}"`);
    expect(urlInput).toContain("basis-full");
    expect(urlInput).toContain("min-h-11");
    expect(urlInput).not.toContain("sm:min-h-8");
  });

  test("renders the feed controller URL through EventsPane", async () => {
    const feedUrl = "https://xmpp.waddle.social/api/calendar/community/v1.community.signature/events.ics";
    const subscriptionHref = "webcal://xmpp.waddle.social/api/calendar/community/v1.community.signature/events.ics";
    const controller: CalendarFeedCopyController = {
      canCopy: computed(() => true),
      copy: async () => {},
      dispose: () => {},
      state: ref("idle"),
      statusLabel: computed(() => ""),
      url: ref(feedUrl),
    };
    const html = await renderVueComponent(
      "../src/components/community/EventsPane.vue",
      props(),
      import.meta.url,
      (app) => app.provide(calendarFeedCopyControllerKey, controller),
    );
    const openLink = elementWithText(html, "a", "Subscribe to calendar");
    const urlInput = elementWithAttribute(html, "input", 'aria-label="Calendar feed URL"');

    expect(openLink).toContain(`href="${subscriptionHref}"`);
    expect(urlInput).toContain(`value="${feedUrl}"`);
  });
});
