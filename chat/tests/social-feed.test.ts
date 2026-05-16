import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useSocialFeed } from "../src/services/social-feed";
import type { BrowserXmppClient, FeedEntry } from "../src/lib/xmpp-client";

const SPACES = "spaces.example.com";

type MockClient = BrowserXmppClient & {
  fetchFeed: ReturnType<typeof mock>;
  publishFeedPost: ReturnType<typeof mock>;
};

function makeClient(entries: FeedEntry[] = []): MockClient {
  return {
    fetchFeed: mock(() => Promise.resolve(entries)),
    publishFeedPost: mock((_jid: string, input: { body: string; title?: string; author?: string }) =>
      Promise.resolve({
        id: "post-new",
        body: input.body,
        ...(input.title ? { title: input.title } : {}),
        ...(input.author ? { author: input.author } : {}),
        publishedMs: Date.parse("2026-06-01T12:00:00Z"),
      } satisfies FeedEntry),
    ),
  } as unknown as MockClient;
}

describe("useSocialFeed", () => {
  test("refresh populates entries sorted newest-first", async () => {
    const client = makeClient([
      { id: "p1", body: "older", publishedMs: 100 },
      { id: "p2", body: "newest", publishedMs: 300 },
      { id: "p3", body: "middle", publishedMs: 200 },
    ]);
    const feed = useSocialFeed(ref<BrowserXmppClient | null>(client), {
      spacesJid: ref<string | null>(SPACES),
    });

    const ok = await feed.refresh();
    expect(ok).toBe(true);
    expect(feed.entries.value.map((e) => e.id)).toEqual(["p2", "p3", "p1"]);
    expect(feed.isLoading.value).toBe(false);
    expect(feed.error.value).toBe(null);
    expect(client.fetchFeed).toHaveBeenCalledWith(SPACES, 50);
  });

  test("post appends locally so the UI shows it without re-fetching", async () => {
    const client = makeClient([]);
    const feed = useSocialFeed(ref<BrowserXmppClient | null>(client), {
      spacesJid: ref<string | null>(SPACES),
    });

    const entry = await feed.post({ body: "hello community" });
    expect(entry?.id).toBe("post-new");
    expect(feed.entries.value.map((e) => e.id)).toContain("post-new");
    expect(client.publishFeedPost).toHaveBeenCalledTimes(1);
    // No auto-refresh after post: avoids races where a refresh
    // overwrites the just-appended entry before the server's items
    // listing reflects the new publish.
    expect(client.fetchFeed).toHaveBeenCalledTimes(0);
  });

  test("post is rejected when body is empty", async () => {
    const client = makeClient([]);
    const feed = useSocialFeed(ref<BrowserXmppClient | null>(client), {
      spacesJid: ref<string | null>(SPACES),
    });
    const entry = await feed.post({ body: "   " });
    expect(entry).toBe(null);
    expect(client.publishFeedPost).not.toHaveBeenCalled();
  });

  test("post records error and leaves entries unchanged on failure", async () => {
    const client = makeClient([{ id: "p1", body: "existing", publishedMs: 100 }]);
    client.publishFeedPost = mock(() => Promise.reject(new Error("Forbidden"))) as unknown as MockClient["publishFeedPost"];

    const feed = useSocialFeed(ref<BrowserXmppClient | null>(client), {
      spacesJid: ref<string | null>(SPACES),
    });
    await feed.refresh();
    const entry = await feed.post({ body: "no-can-do" });
    expect(entry).toBe(null);
    expect(feed.error.value).toBe("Forbidden");
    expect(feed.entries.value.map((e) => e.id)).toEqual(["p1"]);
  });

  test("clear resets state", async () => {
    const client = makeClient([{ id: "p1", body: "x", publishedMs: 1 }]);
    const feed = useSocialFeed(ref<BrowserXmppClient | null>(client), {
      spacesJid: ref<string | null>(SPACES),
    });
    await feed.refresh();
    expect(feed.entries.value.length).toBe(1);
    feed.clear();
    expect(feed.entries.value.length).toBe(0);
    expect(feed.error.value).toBe(null);
    expect(feed.isLoading.value).toBe(false);
  });

  test("refresh skips when spacesJid is null", async () => {
    const client = makeClient([]);
    const feed = useSocialFeed(ref<BrowserXmppClient | null>(client), {
      spacesJid: ref<string | null>(null),
    });
    const ok = await feed.refresh();
    expect(ok).toBe(false);
    expect(client.fetchFeed).not.toHaveBeenCalled();
  });
});
