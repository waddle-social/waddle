import { describe, expect, mock, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import {
  calendarFeedEndpoint,
  calendarFeedSubscriptionHref,
  copyCalendarFeedUrlToClipboard,
  isAllowedCalendarFeedUrl,
  isSameCalendarFeedRequestInput,
  nextCalendarFeedCopyViewState,
  type CalendarFeedFetch,
} from "../src/lib/calendar-feed-url";
import { useCalendarFeedCopy } from "../src/lib/use-calendar-feed-copy";

describe("calendar feed URL helpers", () => {
  test("builds the authenticated helper endpoint with the community JID", () => {
    expect(calendarFeedEndpoint({
      communityJid: "community.example.com",
      serverBaseUrl: "https://server.example.com",
      sessionId: "session-1",
    })).toBe(
      "https://server.example.com/api/calendar/community-feed-url?community_jid=community.example.com",
    );
  });

  test("builds a same-origin helper endpoint when no server base URL is configured", () => {
    expect(calendarFeedEndpoint({
      communityJid: "community.example.com",
      serverBaseUrl: "",
      sessionId: "session-1",
    })).toBe(
      "/api/calendar/community-feed-url?community_jid=community.example.com",
    );
  });

  test("allows safe calendar feed URL schemes and builds subscription hrefs", () => {
    expect(isAllowedCalendarFeedUrl(
      "https://server.example.com/api/calendar/community/token/events.ics",
    )).toBe(true);
    expect(calendarFeedSubscriptionHref(
      "https://server.example.com/api/calendar/community/token/events.ics",
    )).toBe("webcal://server.example.com/api/calendar/community/token/events.ics");
    expect(calendarFeedSubscriptionHref(
      "webcal://server.example.com/api/calendar/community/token/events.ics",
    )).toBe("webcal://server.example.com/api/calendar/community/token/events.ics");
    expect(isAllowedCalendarFeedUrl(
      "http://localhost:8787/api/calendar/community/token/events.ics",
    )).toBe(true);
    expect(isAllowedCalendarFeedUrl(
      "http://server.example.com/api/calendar/community/token/events.ics",
    )).toBe(false);
    expect(isAllowedCalendarFeedUrl("javascript:alert(1)")).toBe(false);
    expect(calendarFeedSubscriptionHref("javascript:alert(1)")).toBeNull();
  });

  test("requests the feed URL with session credentials and copies it", async () => {
    const feedUrl = "https://server.example.com/api/calendar/community/token/events.ics";
    const fetchImpl = mock(async () => new Response(JSON.stringify({ url: feedUrl }))) as CalendarFeedFetch;
    const copyText = mock(async () => {});

    const result = await copyCalendarFeedUrlToClipboard({
      communityJid: "community.example.com",
      serverBaseUrl: "https://server.example.com",
      sessionId: "session-1",
    }, { fetch: fetchImpl, copyText });

    expect(result).toEqual({ status: "copied", url: feedUrl });
    expect(fetchImpl.mock.calls[0]?.[0]).toBe(
      "https://server.example.com/api/calendar/community-feed-url?community_jid=community.example.com",
    );
    expect(fetchImpl.mock.calls[0]?.[1]).toEqual({
      credentials: "include",
      headers: {
        "Accept": "application/json",
        "X-Waddle-Session-Id": "session-1",
      },
    });
    expect(copyText).toHaveBeenCalledWith(feedUrl);
  });

  test("compares feed request context including session changes", () => {
    const base = {
      communityJid: "community.example.com",
      serverBaseUrl: "https://server.example.com",
      sessionId: "session-1",
    };

    expect(isSameCalendarFeedRequestInput(base, { ...base })).toBe(true);
    expect(isSameCalendarFeedRequestInput(base, {
      ...base,
      communityJid: "community.other.example.com",
    })).toBe(false);
    expect(isSameCalendarFeedRequestInput(base, {
      ...base,
      serverBaseUrl: "https://other.example.com",
    })).toBe(false);
    expect(isSameCalendarFeedRequestInput(base, {
      ...base,
      sessionId: "session-2",
    })).toBe(false);
  });

  test("returns the fetched URL when clipboard copy fails", async () => {
    const feedUrl = "https://server.example.com/api/calendar/community/token/events.ics";
    const fetchImpl = mock(async () => new Response(JSON.stringify({ url: feedUrl }))) as CalendarFeedFetch;
    const copyText = mock(async () => {
      throw new Error("denied");
    });

    const result = await copyCalendarFeedUrlToClipboard({
      communityJid: "community.example.com",
      serverBaseUrl: "https://server.example.com",
    }, { fetch: fetchImpl, copyText });

    expect(result).toEqual({ status: "copy_failed", url: feedUrl });
  });

  test("does not attempt clipboard copy when the helper request fails", async () => {
    const fetchImpl = mock(async () => new Response("nope", { status: 500 })) as CalendarFeedFetch;
    const copyText = mock(async () => {});

    const result = await copyCalendarFeedUrlToClipboard({
      communityJid: "community.example.com",
      serverBaseUrl: "https://server.example.com",
    }, { fetch: fetchImpl, copyText });

    expect(result).toEqual({ status: "request_failed" });
    expect(copyText).not.toHaveBeenCalled();
  });

  test("rejects unsafe feed URL schemes before copying or rendering", async () => {
    const fetchImpl = mock(async () =>
      new Response(JSON.stringify({ url: "javascript:alert(document.cookie)" }))) as CalendarFeedFetch;
    const copyText = mock(async () => {});

    const result = await copyCalendarFeedUrlToClipboard({
      communityJid: "community.example.com",
      serverBaseUrl: "https://server.example.com",
    }, { fetch: fetchImpl, copyText });

    expect(result).toEqual({ status: "request_failed" });
    expect(copyText).not.toHaveBeenCalled();
  });

  test("copy view state keeps fetched URLs usable and clears them on retry/context changes", () => {
    const feedUrl = "https://server.example.com/api/calendar/community/token/events.ics";

    const copied = nextCalendarFeedCopyViewState({
      state: "loading",
      url: null,
      result: {
        status: "copied",
        url: feedUrl,
      },
    });

    expect(copied).toEqual({
      state: "copied",
      url: feedUrl,
    });

    const failed = nextCalendarFeedCopyViewState({
      state: "loading",
      url: null,
      result: {
        status: "copy_failed",
        url: feedUrl,
      },
    });

    expect(failed).toEqual({
      state: "error",
      url: feedUrl,
    });
    expect(nextCalendarFeedCopyViewState({
      state: "idle",
      url: failed.url,
      startAttempt: true,
    })).toEqual({ state: "loading", url: null });
    expect(nextCalendarFeedCopyViewState({
      state: "error",
      url: failed.url,
      contextChanged: true,
    })).toEqual({ state: "idle", url: null });
    expect(nextCalendarFeedCopyViewState({
      state: "loading",
      url: failed.url,
      result: { status: "request_failed" },
    })).toEqual({ state: "error", url: null });
  });

  test("copy controller keeps a fetched URL visible after copied status resets", async () => {
    const feedUrl = "https://server.example.com/api/calendar/community/token/events.ics";
    const scope = effectScope();
    const communityJid = ref<string | null>("community.example.com");
    const serverBaseUrl = ref("https://server.example.com");
    const sessionId = ref<string | null>("session-1");
    const fetchImpl = mock(async () => new Response(JSON.stringify({ url: feedUrl }))) as CalendarFeedFetch;
    const copyText = mock(async () => {});
    const controller = scope.run(() =>
      useCalendarFeedCopy({
        communityJid: () => communityJid.value,
        serverBaseUrl: () => serverBaseUrl.value,
        sessionId: () => sessionId.value,
        fetch: fetchImpl,
        copyText,
        resetDelayMs: 0,
      }),
    );
    if (!controller) throw new Error("failed to create calendar feed copy controller");

    await controller.copy();

    expect(fetchImpl.mock.calls[0]?.[0]).toBe(
      "https://server.example.com/api/calendar/community-feed-url?community_jid=community.example.com",
    );
    expect(copyText).toHaveBeenCalledWith(feedUrl);
    expect(controller.state.value).toBe("copied");
    expect(controller.statusLabel.value).toBe("Copied");
    expect(controller.url.value).toBe(feedUrl);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(controller.state.value).toBe("idle");
    expect(controller.statusLabel.value).toBe("");
    expect(controller.url.value).toBe(feedUrl);

    communityJid.value = "community.other.example.com";
    await nextTick();

    expect(controller.state.value).toBe("idle");
    expect(controller.url.value).toBeNull();
    controller.dispose();
    scope.stop();
  });

  test("copy controller can request the feed URL from the same-origin helper", async () => {
    const feedUrl = "https://server.example.com/api/calendar/community/token/events.ics";
    const scope = effectScope();
    const fetchImpl = mock(async () => new Response(JSON.stringify({ url: feedUrl }))) as CalendarFeedFetch;
    const copyText = mock(async () => {});
    const controller = scope.run(() =>
      useCalendarFeedCopy({
        communityJid: () => "community.example.com",
        serverBaseUrl: () => "",
        sessionId: () => "session-1",
        fetch: fetchImpl,
        copyText,
        resetDelayMs: 0,
      }),
    );
    if (!controller) throw new Error("failed to create calendar feed copy controller");

    expect(controller.canCopy.value).toBe(true);
    await controller.copy();

    expect(fetchImpl.mock.calls[0]?.[0]).toBe(
      "/api/calendar/community-feed-url?community_jid=community.example.com",
    );
    expect(controller.state.value).toBe("copied");
    expect(controller.url.value).toBe(feedUrl);
    controller.dispose();
    scope.stop();
  });

  test("copy controller still exposes the fetched URL after clipboard failure status resets", async () => {
    const feedUrl = "https://server.example.com/api/calendar/community/token/events.ics";
    const scope = effectScope();
    const controller = scope.run(() =>
      useCalendarFeedCopy({
        communityJid: () => "community.example.com",
        serverBaseUrl: () => "https://server.example.com",
        sessionId: () => null,
        fetch: mock(async () => new Response(JSON.stringify({ url: feedUrl }))) as CalendarFeedFetch,
        copyText: mock(async () => {
          throw new Error("clipboard denied");
        }),
        resetDelayMs: 0,
      }),
    );
    if (!controller) throw new Error("failed to create calendar feed copy controller");

    await controller.copy();

    expect(controller.state.value).toBe("error");
    expect(controller.statusLabel.value).toBe("Couldn't copy");
    expect(controller.url.value).toBe(feedUrl);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(controller.state.value).toBe("idle");
    expect(controller.statusLabel.value).toBe("");
    expect(controller.url.value).toBe(feedUrl);
    controller.dispose();
    scope.stop();
  });
});
