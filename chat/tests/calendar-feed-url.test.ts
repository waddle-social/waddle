import { describe, expect, mock, test } from "bun:test";
import {
  calendarFeedEndpoint,
  copyCalendarFeedUrlToClipboard,
  isSameCalendarFeedRequestInput,
  nextCalendarFeedCopyViewState,
  type CalendarFeedFetch,
} from "../src/lib/calendar-feed-url";

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

  test("copy view state clears stale fallback URLs on retry and context changes", () => {
    const failed = nextCalendarFeedCopyViewState({
      state: "loading",
      fallbackUrl: null,
      result: {
        status: "copy_failed",
        url: "https://server.example.com/api/calendar/community/token/events.ics",
      },
    });

    expect(failed).toEqual({
      state: "error",
      fallbackUrl: "https://server.example.com/api/calendar/community/token/events.ics",
    });
    expect(nextCalendarFeedCopyViewState({
      state: "idle",
      fallbackUrl: failed.fallbackUrl,
      startAttempt: true,
    })).toEqual({ state: "loading", fallbackUrl: null });
    expect(nextCalendarFeedCopyViewState({
      state: "error",
      fallbackUrl: failed.fallbackUrl,
      contextChanged: true,
    })).toEqual({ state: "idle", fallbackUrl: null });
    expect(nextCalendarFeedCopyViewState({
      state: "loading",
      fallbackUrl: failed.fallbackUrl,
      result: { status: "request_failed" },
    })).toEqual({ state: "error", fallbackUrl: null });
  });
});
