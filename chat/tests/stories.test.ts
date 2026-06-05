import { describe, expect, mock, test } from "bun:test";
import { effectScope, ref } from "vue";
import { useStories } from "../src/services/stories";
import type { BrowserXmppClient, Story } from "../src/lib/xmpp-client";

const COMMUNITY = "community.example.com";

type MockClient = BrowserXmppClient & {
  fetchStories: ReturnType<typeof mock>;
  publishStory: ReturnType<typeof mock>;
  fetchStoryReactions: ReturnType<typeof mock>;
  publishStoryReactions: ReturnType<typeof mock>;
  retractStoryReactions: ReturnType<typeof mock>;
  bareJid: string;
};

function makeClient(stories: Story[] = []): MockClient {
  return {
    bareJid: "alice@example.com",
    fetchStories: mock(() => Promise.resolve(stories)),
    fetchStoryReactions: mock((_: string, storyId: string) =>
      Promise.resolve(storyId === "s1" ? [
        { jid: "bob@example.com", emojis: ["👍"], unknownChildrenXml: [] },
        { jid: "alice@example.com", emojis: ["❤️"], unknownChildrenXml: ["<future xmlns=\"urn:test\"/>"] },
      ] : []),
    ),
    publishStoryReactions: mock(() => Promise.resolve()),
    retractStoryReactions: mock(() => Promise.resolve()),
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
  test("refresh filters expired and sorts newest-first", async () => {
    const now = Date.parse("2026-06-01T12:00:00Z");
    const past = now - 10 * 60_000;
    const future = now + 10 * 60_000;
    const client = makeClient([
      { id: "expired", body: "old", postedMs: now - 60_000, expiresMs: past },
      { id: "later", body: "later", postedMs: now - 30_000, expiresMs: future },
      { id: "newest", body: "newest", postedMs: now, expiresMs: future },
    ]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { communityJid: ref<string | null>(COMMUNITY) }),
    );
    story.nowMs.value = now;
    await story.refresh();
    expect(story.activeStories.value.map((s) => s.id)).toEqual(["newest", "later"]);
  });

  test("post appends locally and skips empty input", async () => {
    const client = makeClient([]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { communityJid: ref<string | null>(COMMUNITY) }),
    );
    story.nowMs.value = Date.parse("2026-06-01T12:00:00Z");
    const empty = await story.post({ body: "  " });
    expect(empty).toBe(null);
    expect(client.publishStory).not.toHaveBeenCalled();
    const posted = await story.post({ body: "hi", author: "alice@example.com" });
    expect(posted?.id).toBe("new-story");
    expect(story.activeStories.value.map((s) => s.id)).toContain("new-story");
  });

  test("nowMs advancement drops just-expired stories", async () => {
    const t0 = Date.parse("2026-06-01T12:00:00Z");
    const client = makeClient([
      { id: "soon", body: "soon", postedMs: t0, expiresMs: t0 + 60_000 },
      { id: "later", body: "later", postedMs: t0, expiresMs: t0 + 3_600_000 },
    ]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { communityJid: ref<string | null>(COMMUNITY) }),
    );
    story.nowMs.value = t0;
    await story.refresh();
    expect(story.activeStories.value.length).toBe(2);
    story.nowMs.value = t0 + 2 * 60_000;
    expect(story.activeStories.value.map((s) => s.id)).toEqual(["later"]);
  });

  test("reactions aggregate, toggle optimistically, preserve unknown children, and roll back", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { communityJid: ref<string | null>(COMMUNITY) }),
    );
    await story.refresh();

    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 1, "❤️": 1 });
    expect(story.reactionSummary("s1").mine).toEqual(["❤️"]);

    await story.toggleReaction("s1", "👍");
    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 2, "❤️": 1 });
    expect(client.publishStoryReactions).toHaveBeenLastCalledWith(
      COMMUNITY,
      "s1",
      ["❤️", "👍"],
      ["<future xmlns=\"urn:test\"/>"],
    );

    client.publishStoryReactions = mock(() => Promise.reject(new Error("forbidden"))) as unknown as MockClient["publishStoryReactions"];
    const ok = await story.toggleReaction("s1", "🎉");
    expect(ok).toBe(false);
    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 2, "❤️": 1 });

    client.publishStoryReactions = mock(() => Promise.resolve()) as unknown as MockClient["publishStoryReactions"];
    await story.toggleReaction("s1", "❤️");
    await story.toggleReaction("s1", "👍");
    expect(client.retractStoryReactions).toHaveBeenCalledWith(COMMUNITY, "s1");
  });

  test("stale reaction refresh results do not overwrite newer refresh state", async () => {
    let resolveFirst!: (items: Array<{ jid: string; emojis: string[]; unknownChildrenXml: string[] }>) => void;
    const firstReactionFetch = new Promise<Array<{ jid: string; emojis: string[]; unknownChildrenXml: string[] }>>((resolve) => {
      resolveFirst = resolve;
    });
    let reactionFetchCount = 0;
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchStoryReactions = mock(() => {
      reactionFetchCount += 1;
      if (reactionFetchCount === 1) return firstReactionFetch;
      return Promise.resolve([{ jid: "carol@example.com", emojis: ["🎉"], unknownChildrenXml: [] }]);
    }) as unknown as MockClient["fetchStoryReactions"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { communityJid: ref<string | null>(COMMUNITY) }),
    );
    const first = story.refresh();
    await Promise.resolve();
    const second = story.refresh();
    await second;
    expect(story.reactionSummary("s1").counts).toEqual({ "🎉": 1 });

    resolveFirst([{ jid: "bob@example.com", emojis: ["👍"], unknownChildrenXml: [] }]);
    await first;
    expect(story.reactionSummary("s1").counts).toEqual({ "🎉": 1 });
  });

  test("reaction refresh bounds concurrent IQ fetches", async () => {
    let active = 0;
    let maxActive = 0;
    const client = makeClient(
      Array.from({ length: 20 }, (_, index) => ({ id: `s${index}`, body: "story", postedMs: index })),
    );
    client.fetchStoryReactions = mock(async () => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      await Promise.resolve();
      active -= 1;
      return [];
    }) as unknown as MockClient["fetchStoryReactions"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { communityJid: ref<string | null>(COMMUNITY) }),
    );
    await story.refresh();

    expect(maxActive).toBeLessThanOrEqual(8);
  });
});
