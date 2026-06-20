import { describe, expect, test } from "bun:test";
import { computed, ref } from "vue";
import { renderVueComponent, renderVueComponentSource } from "./helpers/render-vue-sfc";
import {
  calendarFeedCopyControllerKey,
  type CalendarFeedCopyController,
} from "../src/lib/use-calendar-feed-copy";
import {
  dateTimeValue,
  dateValue,
  localDateString,
} from "../src/lib/xmpp-client";

const chatReadyShellSource = await Bun.file(
  new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url),
).text();

const eventsPaneSource = await Bun.file(
  new URL("../src/components/community/EventsPane.vue", import.meta.url),
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

  test("composer source keeps 30-minute duration default, all-day controls, and validation copy", () => {
    expect(eventsPaneSource).toContain('const durationChoice = ref("30")');
    expect(eventsPaneSource).toContain('{ value: "30", label: "30 minutes" }');
    expect(eventsPaneSource).toContain('{ value: "60", label: "1 hour" }');
    expect(eventsPaneSource).toContain('{ value: "custom", label: "Custom" }');
    expect(eventsPaneSource).toContain("All day");
    expect(eventsPaneSource).toContain('v-model="allDayStart"');
    expect(eventsPaneSource).toContain('v-model="allDayEnd"');
    expect(eventsPaneSource).toContain("setDefaultAllDayDates()");
    expect(eventsPaneSource).toContain("visibleComposerError");
    expect(eventsPaneSource).toContain('return "End date must be after the start date."');
    expect(eventsPaneSource).toContain('return "End time must be after the start time."');
    expect(eventsPaneSource).toContain('return "All-day repeats need a count instead of an until date."');
  });

  test("renders multi-day all-day events in every covered month cell and selected day list", async () => {
    const now = new Date();
    const start = localDateString(now.getFullYear(), now.getMonth(), 1);
    const nextMonth = new Date(now.getFullYear(), now.getMonth() + 1, 1);
    const endExclusive = localDateString(
      nextMonth.getFullYear(),
      nextMonth.getMonth(),
      nextMonth.getDate(),
    );
    const html = await renderVueComponent(
      "../src/components/community/EventsPane.vue",
      props({
        events: [{
          id: "expo",
          uid: "expo",
          summary: "Three Day Expo",
          organizer: "xmpp:alice@example.com",
          dtstart: dateValue(start),
          dtend: dateValue(endExclusive),
        }],
        selfJid: "alice@example.com",
      }),
      import.meta.url,
    );

    expect(html.match(/Three Day Expo/g)?.length ?? 0).toBeGreaterThanOrEqual(3);
    expect(html).toContain("All day");
  });

  test("renders ongoing timed spans on a later visible day", async () => {
    const now = new Date();
    const startMs = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1, 23, 0).getTime();
    const endMs = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 1, 0).getTime();
    const html = await renderVueComponent(
      "../src/components/community/EventsPane.vue",
      props({
        events: [{
          id: "overnight",
          uid: "overnight",
          summary: "Overnight Window",
          organizer: "xmpp:alice@example.com",
          dtstart: dateTimeValue(startMs),
          dtend: dateTimeValue(endMs),
        }],
        selfJid: "alice@example.com",
      }),
      import.meta.url,
    );

    expect(html).toContain("continues Overnight Window");
    expect(html).toContain("Overnight Window");
  });
});
