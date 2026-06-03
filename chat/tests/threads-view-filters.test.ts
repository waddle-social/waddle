import { describe, expect, test } from "bun:test";
import {
  activeSinceIso,
  decodeThreadsFilterState,
  encodeThreadsFilterState,
  threadDisplayTitle,
} from "../src/lib/threads-view-filters";

describe("threads view filters", () => {
  test("decodes supported URL params and falls back on invalid values", () => {
    expect(decodeThreadsFilterState("?status=unread&active=14d&channel=chat&q=push&sort=replies")).toEqual({
      status: "unread",
      active: "14d",
      channel: "chat",
      query: "push",
      sort: "replies",
    });
    expect(decodeThreadsFilterState("?status=stale&active=99d&sort=hot")).toEqual({
      status: "unread",
      active: "7d",
      channel: "all",
      query: "",
      sort: "recent",
    });
  });

  test("encodes only non-default URL params", () => {
    expect(
      encodeThreadsFilterState({
        status: "following",
        active: "30d",
        channel: "chat",
        query: " notifications ",
        sort: "unread",
      }),
    ).toBe("status=following&active=30d&channel=chat&q=notifications&sort=unread");
    // The default view (unread / 7d) encodes to an empty query string.
    expect(
      encodeThreadsFilterState({
        status: "unread",
        active: "7d",
        channel: "all",
        query: "",
        sort: "recent",
      }),
    ).toBe("");
    // Widening back to all-statuses / all-time is now non-default, so it
    // round-trips through the URL.
    expect(
      encodeThreadsFilterState({
        status: "all",
        active: "all",
        channel: "all",
        query: "",
        sort: "recent",
      }),
    ).toBe("status=all&active=all");
  });

  test("computes active-since timestamps from fixed windows", () => {
    const now = new Date("2026-06-02T12:00:00.000Z");
    expect(activeSinceIso("7d", now)).toBe("2026-05-26T00:00:00.000Z");
    expect(activeSinceIso("14d", now)).toBe("2026-05-19T00:00:00.000Z");
    expect(activeSinceIso("30d", now)).toBe("2026-05-03T00:00:00.000Z");
    expect(activeSinceIso("all", now)).toBeUndefined();
  });

  test("normalizes noisy previews for thread rows", () => {
    expect(threadDisplayTitle({ preview: "> Notifications failed" })).toBe("Notifications failed");
    expect(threadDisplayTitle({ preview: "https://media3.giphy.com/media/example/giphy.gif" })).toBe("Media attachment");
    expect(threadDisplayTitle({ preview: "https://example.com/cat.webp" })).toBe("Media attachment");
    expect(threadDisplayTitle({})).toBe("Thread");
  });
});
