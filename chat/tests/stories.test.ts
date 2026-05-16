import { describe, expect, mock, test } from "bun:test";
import { effectScope, ref } from "vue";
import { useStories } from "../src/services/stories";
import type { BrowserXmppClient, Story } from "../src/lib/xmpp-client";

const SPACES = "spaces.example.com";

type MockClient = BrowserXmppClient & {
  fetchStories: ReturnType<typeof mock>;
  publishStory: ReturnType<typeof mock>;
};

function makeClient(stories: Story[] = []): MockClient {
  return {
    fetchStories: mock(() => Promise.resolve(stories)),
    publishStory: mock((_jid: string, input: { body?: string; mediaUrl?: string; author?: string }) =>
      Promise.resolve({
        id: "new-story",
        ...(input.body ? { body: input.body } : {}),
        ...(input.mediaUrl ? { mediaUrl: input.mediaUrl } : {}),
        ...(input.author ? { author: input.author } : {}),
        postedMs: Date.parse("2026-06-01T12:00:00Z"),
        expiresMs: Date.parse("2026-06-02T12:00:00Z"),
      } satisfies Story),
    ),
  } as unknown as MockClient;
}

function withScope<T>(fn: () => T): T {
  const scope = effectScope();
  let result!: T;
  scope.run(() => {
    result = fn();
  });
  return result;
}

describe("useStories", () => {
  test("refresh filters expired stories and sorts newest-first", async () => {
    const now = Date.parse("2026-06-01T12:00:00Z");
    const future = now + 10 * 60_000;
    const past = now - 10 * 60_000;
    const client = makeClient([
      { id: "expired", body: "old", postedMs: now - 60_000, expiresMs: past },
      { id: "later", body: "newer", postedMs: now - 30_000, expiresMs: future },
      { id: "newest", body: "newest", postedMs: now, expiresMs: future },
    ]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), {
        spacesJid: ref<string | null>(SPACES),
      }),
    );
    story.nowMs.value = now;

    await story.refresh();
    expect(story.activeStories.value.map((s) => s.id)).toEqual(["newest", "later"]);
  });

  test("post appends locally and skips empty input", async () => {
    const client = makeClient([]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), {
        spacesJid: ref<string | null>(SPACES),
      }),
    );
    story.nowMs.value = Date.parse("2026-06-01T12:00:00Z");

    const empty = await story.post({ body: "  " });
    expect(empty).toBe(null);
    expect(client.publishStory).not.toHaveBeenCalled();

    const posted = await story.post({ body: "hi", author: "alice@example.com" });
    expect(posted?.id).toBe("new-story");
    expect(story.activeStories.value.map((s) => s.id)).toContain("new-story");
  });

  test("post error surfaces and entries stay intact", async () => {
    const now = Date.parse("2026-06-01T12:00:00Z");
    const future = now + 60_000;
    const client = makeClient([
      { id: "existing", body: "x", postedMs: now, expiresMs: future },
    ]);
    client.publishStory = mock(() => Promise.reject(new Error("Forbidden"))) as unknown as MockClient["publishStory"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), {
        spacesJid: ref<string | null>(SPACES),
      }),
    );
    story.nowMs.value = now;
    await story.refresh();

    const failed = await story.post({ body: "no-can-do" });
    expect(failed).toBe(null);
    expect(story.error.value).toBe("Forbidden");
    expect(story.activeStories.value.map((s) => s.id)).toEqual(["existing"]);
  });

  test("clear resets state and prevents stale refresh from landing", async () => {
    const now = Date.parse("2026-06-01T12:00:00Z");
    const client = makeClient([
      { id: "x", body: "x", postedMs: now, expiresMs: now + 60_000 },
    ]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), {
        spacesJid: ref<string | null>(SPACES),
      }),
    );
    story.nowMs.value = now;
    await story.refresh();
    expect(story.activeStories.value.length).toBe(1);

    story.clear();
    expect(story.activeStories.value.length).toBe(0);
  });

  test("expiry filtering re-evaluates as nowMs advances", async () => {
    const t0 = Date.parse("2026-06-01T12:00:00Z");
    const client = makeClient([
      { id: "soon", body: "soon", postedMs: t0, expiresMs: t0 + 60_000 },
      { id: "later", body: "later", postedMs: t0, expiresMs: t0 + 3_600_000 },
    ]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), {
        spacesJid: ref<string | null>(SPACES),
      }),
    );
    story.nowMs.value = t0;
    await story.refresh();
    expect(story.activeStories.value.length).toBe(2);

    // Advance past "soon" expiry: it should drop out of active list.
    story.nowMs.value = t0 + 2 * 60_000;
    expect(story.activeStories.value.map((s) => s.id)).toEqual(["later"]);
  });
});
