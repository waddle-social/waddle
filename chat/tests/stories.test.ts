import { describe, expect, mock, test } from "bun:test";
import { effectScope, ref } from "vue";
import { useStories } from "../src/services/stories";
import type { BrowserXmppClient, PubsubEvent, Story, StoryReactionItem, StoryReactionSummary } from "../src/lib/xmpp-client";

/** The story author: each account hosts its own stories PEP node. */
const AUTHOR = "bob@example.com";

type MockClient = BrowserXmppClient & {
  fetchStories: ReturnType<typeof mock>;
  publishStory: ReturnType<typeof mock>;
  fetchStoryReactions: ReturnType<typeof mock>;
  fetchMyStoryReactions: ReturnType<typeof mock>;
  fetchStoryReactionSummary: ReturnType<typeof mock>;
  publishStoryReactions: ReturnType<typeof mock>;
  retractStoryReactions: ReturnType<typeof mock>;
  setPubsubEventHandler: ReturnType<typeof mock>;
  addPubsubEventHandler: ReturnType<typeof mock>;
  emitPubsubEvent: (event: PubsubEvent) => void;
  bareJid: string;
};

function makeClient(stories: Story[] = []): MockClient {
  const reactionItems = (storyId: string): StoryReactionItem[] => storyId === "s1" ? [
    { jid: "bob@example.com", emojis: ["👍"], unknownChildrenXml: [] },
    { jid: "alice@example.com", emojis: ["❤️"], unknownChildrenXml: ["<future xmlns=\"urn:test\"/>"] },
  ] : [];
  const reactionSummary = (storyId: string): StoryReactionSummary => storyId === "s1"
    ? { counts: { "👍": 1, "❤️": 1 }, reactors: {}, mine: [] }
    : { counts: {}, reactors: {}, mine: [] };
  const pubsubEventHandlers = new Set<(event: PubsubEvent) => void>();
  const client = {
    bareJid: "alice@example.com",
    fetchStories: mock(() => Promise.resolve(stories)),
    fetchStoryReactions: mock((_: string, storyId: string) => Promise.resolve(reactionItems(storyId))),
    fetchMyStoryReactions: mock((_: string, storyId: string) =>
      Promise.resolve(reactionItems(storyId).find((item) => item.jid === "alice@example.com") ?? null),
    ),
    publishStoryReactions: mock(() => Promise.resolve()),
    retractStoryReactions: mock(() => Promise.resolve()),
    setPubsubEventHandler: mock((handler: ((event: PubsubEvent) => void) | null) => {
      pubsubEventHandlers.clear();
      if (handler) pubsubEventHandlers.add(handler);
    }),
    addPubsubEventHandler: mock((handler: (event: PubsubEvent) => void) => {
      pubsubEventHandlers.add(handler);
      return () => pubsubEventHandlers.delete(handler);
    }),
    emitPubsubEvent: (event: PubsubEvent) => {
      for (const handler of pubsubEventHandlers) handler(event);
    },
    publishStory: mock((input: { body?: string; mediaUrl: string; author?: string }) =>
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
  client.fetchStoryReactionSummary = mock((_: string, storyId: string) => Promise.resolve(reactionSummary(storyId))) as unknown as MockClient["fetchStoryReactionSummary"];
  return client;
}

/** Let the in-flight refresh get past its story fetch, so the stories' hosts are known. */
async function settleFetch(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
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
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    story.nowMs.value = now;
    await story.refresh();
    expect(story.activeStories.value.map((s) => s.id)).toEqual(["newest", "later"]);
  });

  test("refresh reads every author's own stories node and routes reactions to the story's host", async () => {
    const now = Date.parse("2026-06-01T12:00:00Z");
    const future = now + 10 * 60_000;
    const client = makeClient([]);
    client.fetchStories = mock((author: string) =>
      author === "carol@example.com"
        ? Promise.resolve([{ id: "carol-story", body: "c", postedMs: now - 1000, expiresMs: future }])
        : author === "dave@example.com"
          ? Promise.reject(new Error("presence subscription required"))
          : Promise.resolve([{ id: "bob-story", body: "b", postedMs: now, expiresMs: future }]),
    ) as unknown as MockClient["fetchStories"];
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), {
        authorJids: ref<readonly string[]>([AUTHOR, "carol@example.com", "dave@example.com"]),
      }),
    );
    story.nowMs.value = now;
    await story.refresh();

    expect(story.activeStories.value.map((s) => s.id)).toEqual(["bob-story", "carol-story"]);
    expect(client.fetchStoryReactionSummary).toHaveBeenCalledWith("carol@example.com", "carol-story");
    expect(client.fetchStoryReactionSummary).toHaveBeenCalledWith(AUTHOR, "bob-story");
    // An unreadable contact node is the empty case, not a failure of the rail.
    expect(story.error.value).toBeNull();

    await story.toggleReaction("carol-story", "👍");
    expect(client.publishStoryReactions).toHaveBeenLastCalledWith(
      "carol@example.com",
      "carol-story",
      ["👍"],
      [],
    );
  });

  test("a story published on the user's own node is hosted by the user", async () => {
    const client = makeClient([]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>(["alice@example.com"]) }),
    );
    const posted = await story.post({ mediaUrl: "https://example.com/story.jpg" });
    expect(posted?.id).toBe("new-story");
    expect(client.publishStory).toHaveBeenCalledWith({ mediaUrl: "https://example.com/story.jpg" });
    await story.toggleReaction("new-story", "❤️");
    expect(client.publishStoryReactions).toHaveBeenLastCalledWith("alice@example.com", "new-story", ["❤️"], []);
  });

  test("post appends locally and skips empty input", async () => {
    const client = makeClient([]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    story.nowMs.value = Date.parse("2026-06-01T12:00:00Z");
    const empty = await story.post({ body: "  ", mediaUrl: "  " });
    expect(empty).toBe(null);
    expect(client.publishStory).not.toHaveBeenCalled();
    const bodyOnly = await story.post({ body: "hi", mediaUrl: "  ", author: "alice@example.com" });
    expect(bodyOnly).toBe(null);
    expect(client.publishStory).not.toHaveBeenCalled();
    const posted = await story.post({
      body: "hi",
      mediaUrl: "https://example.com/story.jpg",
      mediaType: "image/jpeg",
      author: "alice@example.com",
    });
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
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    story.nowMs.value = t0;
    await story.refresh();
    expect(story.activeStories.value.length).toBe(2);
    story.nowMs.value = t0 + 2 * 60_000;
    expect(story.activeStories.value.map((s) => s.id)).toEqual(["later"]);
  });

  test("reaction summaries load counts, toggle optimistically, and roll back", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();

    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 1, "❤️": 1 });
    expect(story.reactionSummary("s1").mine).toEqual(["❤️"]);

    await story.toggleReaction("s1", "👍");
    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 2, "❤️": 1 });
    expect(client.publishStoryReactions).toHaveBeenLastCalledWith(
      AUTHOR,
      "s1",
      ["❤️", "👍"],
      ["<future xmlns=\"urn:test\"/>"],
    );

    client.publishStoryReactions = mock(() => Promise.reject(new Error("forbidden"))) as unknown as MockClient["publishStoryReactions"];
    const ok = await story.toggleReaction("s1", "🎉");
    expect(ok).toBe(false);
    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 2, "❤️": 1 });

    client.publishStoryReactions = mock(() => Promise.resolve()) as unknown as MockClient["publishStoryReactions"];
    await story.toggleReaction("s1", "👍");
    await story.toggleReaction("s1", "❤️");
    expect(client.retractStoryReactions).toHaveBeenCalledWith(AUTHOR, "s1");
  });

  test("refresh reads story counts from summary payloads instead of raw attachments", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchStoryReactions = mock(() => {
      throw new Error("raw attachments should not load counts");
    }) as unknown as MockClient["fetchStoryReactions"];
    client.fetchStoryReactionSummary = mock(() =>
      Promise.resolve({ counts: { "👍": 3 }, reactors: {}, mine: [], noticedCount: 2 }),
    ) as unknown as MockClient["fetchStoryReactionSummary"];
    client.fetchMyStoryReactions = mock(() => Promise.resolve(null)) as unknown as MockClient["fetchMyStoryReactions"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();

    expect(client.fetchStoryReactionSummary).toHaveBeenCalledWith(AUTHOR, "s1");
    expect(client.fetchStories).toHaveBeenCalledWith(AUTHOR, 50);
    expect(client.fetchStoryReactions).not.toHaveBeenCalled();
    expect(story.reactionSummary("s1")).toEqual({
      counts: { "👍": 3 },
      reactors: {},
      mine: [],
      noticedCount: 2,
    });
  });

  test("typed summary pubsub event updates the story reaction store", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();

    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        payload: {
          kind: "attachmentSummary",
          reactions: [{ emoji: "🎉", count: 4, reactors: ["alice@example.com", "bob@example.com"] }],
          noticedCount: 3,
        },
      }],
    });

    expect(story.reactionSummary("s1")).toEqual({
      counts: { "🎉": 4 },
      reactors: { "🎉": ["alice@example.com", "bob@example.com"] },
      mine: ["🎉"],
      noticedCount: 3,
    });

    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:example:unrelated",
      items: [{ id: "s1", payload: { kind: "opaque", xml: "<future/>" } }],
    });
    expect(story.reactionSummary("s1").counts).toEqual({ "🎉": 4 });
  });

  test("typed story retract pubsub event removes the story and its reactions", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();
    expect(story.activeStories.value.map((item) => item.id)).toEqual(["s1"]);
    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 1, "❤️": 1 });

    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        retracted: true,
        payload: { kind: "empty" },
      }],
    });

    expect(story.activeStories.value).toEqual([]);
    expect(story.reactionSummary("s1")).toEqual({ counts: {}, reactors: {}, mine: [] });
  });

  test("typed summary retract pubsub event removes reaction state", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();
    expect(story.activeStories.value.map((item) => item.id)).toEqual(["s1"]);
    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 1, "❤️": 1 });

    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        retracted: true,
        payload: { kind: "empty" },
      }],
    });

    expect(story.activeStories.value.map((item) => item.id)).toEqual(["s1"]);
    expect(story.reactionSummary("s1")).toEqual({ counts: {}, reactors: {}, mine: [] });
  });

  test("typed summary pubsub event preserves pending optimistic self reactions", async () => {
    let resolvePublish!: () => void;
    const pendingPublish = new Promise<void>((resolve) => {
      resolvePublish = resolve;
    });
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.publishStoryReactions = mock(() => pendingPublish) as unknown as MockClient["publishStoryReactions"];
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();
    const toggle = story.toggleReaction("s1", "👍");
    await settleFetch();

    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        payload: {
          kind: "attachmentSummary",
          reactions: [
            { emoji: "❤️", count: 1, reactors: ["alice@example.com"] },
            { emoji: "🔥", count: 1, reactors: ["bob@example.com"] },
          ],
        },
      }],
    });

    expect(story.reactionSummary("s1").counts).toEqual({ "❤️": 1, "🔥": 1, "👍": 1 });
    expect(story.reactionSummary("s1").mine).toEqual(["❤️", "👍"]);
    resolvePublish();
    await toggle;
  });

  test("typed summary pubsub event uses canonical counts after local publish settles", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();
    await story.toggleReaction("s1", "👍");

    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        payload: {
          kind: "attachmentSummary",
          reactions: [{ emoji: "🔥", count: 1, reactors: ["bob@example.com"] }],
        },
      }],
    });

    expect(story.reactionSummary("s1").counts).toEqual({ "🔥": 1 });
    expect(story.reactionSummary("s1").mine).toEqual([]);
  });

  test("live summary pubsub event is not clobbered by in-flight refresh results", async () => {
    let resolveSummary!: (summary: StoryReactionSummary) => void;
    const pendingSummary = new Promise<StoryReactionSummary>((resolve) => {
      resolveSummary = resolve;
    });
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchStoryReactionSummary = mock(() => pendingSummary) as unknown as MockClient["fetchStoryReactionSummary"];
    client.fetchMyStoryReactions = mock(() => Promise.resolve(null)) as unknown as MockClient["fetchMyStoryReactions"];
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );

    const refresh = story.refresh();
    await settleFetch();
    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        payload: {
          kind: "attachmentSummary",
          reactions: [{ emoji: "🔥", count: 2, reactors: ["bob@example.com"] }],
        },
      }],
    });
    resolveSummary({ counts: { "👍": 1 }, reactors: {}, mine: [] });
    await refresh;

    expect(story.reactionSummary("s1").counts).toEqual({ "🔥": 2 });
    expect(story.reactionSummary("s1").mine).toEqual([]);
  });

  test("pubsub event handlers are additive and scope cleanup removes only its handler", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const firstScope = effectScope();
    const secondScope = effectScope();
    const first = firstScope.run(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    )!;
    const second = secondScope.run(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    )!;
    await first.refresh();
    await second.refresh();

    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        payload: {
          kind: "attachmentSummary",
          reactions: [{ emoji: "🎉", count: 2, reactors: [] }],
        },
      }],
    });
    expect(first.reactionSummary("s1").counts).toEqual({ "🎉": 2 });
    expect(second.reactionSummary("s1").counts).toEqual({ "🎉": 2 });

    firstScope.stop();
    client.emitPubsubEvent({
      from: AUTHOR,
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        payload: {
          kind: "attachmentSummary",
          reactions: [{ emoji: "🔥", count: 1, reactors: [] }],
        },
      }],
    });
    expect(first.reactionSummary("s1").counts).toEqual({ "🎉": 2 });
    expect(second.reactionSummary("s1").counts).toEqual({ "🔥": 1 });
    secondScope.stop();
  });

  test("summary pubsub events are scoped to the active community", async () => {
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();

    client.emitPubsubEvent({
      from: "other.example.com",
      node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0",
      items: [{
        id: "s1",
        payload: {
          kind: "attachmentSummary",
          reactions: [{ emoji: "🔥", count: 9, reactors: [] }],
        },
      }],
    });

    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 1, "❤️": 1 });
  });

  test("stale reaction refresh results do not overwrite newer refresh state", async () => {
    let resolveFirst!: (summary: StoryReactionSummary) => void;
    const firstSummaryFetch = new Promise<StoryReactionSummary>((resolve) => {
      resolveFirst = resolve;
    });
    let reactionFetchCount = 0;
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchMyStoryReactions = mock(() => Promise.resolve(null)) as unknown as MockClient["fetchMyStoryReactions"];
    client.fetchStoryReactionSummary = mock(() => {
      reactionFetchCount += 1;
      if (reactionFetchCount === 1) return firstSummaryFetch;
      return Promise.resolve({ counts: { "🎉": 1 }, reactors: {}, mine: [] });
    }) as unknown as MockClient["fetchStoryReactionSummary"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    const first = story.refresh();
    for (let i = 0; i < 5 && reactionFetchCount === 0; i += 1) {
      await Promise.resolve();
    }
    expect(reactionFetchCount).toBe(1);
    const second = story.refresh();
    await second;
    expect(story.reactionSummary("s1").counts).toEqual({ "🎉": 1 });

    resolveFirst({ counts: { "👍": 1 }, reactors: {}, mine: [] });
    await first;
    expect(story.reactionSummary("s1").counts).toEqual({ "🎉": 1 });
  });

  test("reaction refresh preserves optimistic toggle made while batches are pending", async () => {
    let resolveReactions!: (summary: StoryReactionSummary) => void;
    const pendingReactions = new Promise<StoryReactionSummary>((resolve) => {
      resolveReactions = resolve;
    });
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchMyStoryReactions = mock(() => Promise.resolve(null)) as unknown as MockClient["fetchMyStoryReactions"];
    client.fetchStoryReactionSummary = mock(() => pendingReactions) as unknown as MockClient["fetchStoryReactionSummary"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    const refresh = story.refresh();
    await settleFetch();
    await story.toggleReaction("s1", "🎉");
    expect(story.reactionSummary("s1").counts).toEqual({ "🎉": 1 });

    resolveReactions({ counts: { "👍": 1 }, reactors: {}, mine: [] });
    await refresh;

    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 1, "🎉": 1 });
    expect(story.reactionSummary("s1").mine).toEqual(["🎉"]);
  });

  test("reaction refresh does not resurrect a locally removed self item", async () => {
    let resolveSummary!: (summary: StoryReactionSummary) => void;
    const pendingSummary = new Promise<StoryReactionSummary>((resolve) => {
      resolveSummary = resolve;
    });
    const staleSelf = { jid: "alice@example.com", emojis: ["❤️"], unknownChildrenXml: [] };
    let fetchCount = 0;
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchStoryReactionSummary = mock(() => {
      fetchCount += 1;
      return fetchCount === 1
        ? Promise.resolve({ counts: { "❤️": 1 }, reactors: {}, mine: [] })
        : pendingSummary;
    }) as unknown as MockClient["fetchStoryReactionSummary"];
    client.fetchMyStoryReactions = mock(() => Promise.resolve(staleSelf)) as unknown as MockClient["fetchMyStoryReactions"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();
    expect(story.reactionSummary("s1").mine).toEqual(["❤️"]);

    const refresh = story.refresh();
    await settleFetch();
    await story.toggleReaction("s1", "❤️");
    expect(story.reactionSummary("s1").mine).toEqual([]);

    resolveSummary({ counts: { "❤️": 1 }, reactors: {}, mine: [] });
    await refresh;

    expect(story.reactionSummary("s1").counts).toEqual({});
    expect(story.reactionSummary("s1").mine).toEqual([]);
  });

  test("reaction refresh applies self delta instead of double-counting old self reactions", async () => {
    let resolveSummary!: (summary: StoryReactionSummary) => void;
    const pendingSummary = new Promise<StoryReactionSummary>((resolve) => {
      resolveSummary = resolve;
    });
    const staleSelf = { jid: "alice@example.com", emojis: ["❤️"], unknownChildrenXml: [] };
    let fetchCount = 0;
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchStoryReactionSummary = mock(() => {
      fetchCount += 1;
      return fetchCount === 1
        ? Promise.resolve({ counts: { "❤️": 1 }, reactors: {}, mine: [] })
        : pendingSummary;
    }) as unknown as MockClient["fetchStoryReactionSummary"];
    client.fetchMyStoryReactions = mock(() => Promise.resolve(staleSelf)) as unknown as MockClient["fetchMyStoryReactions"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();
    expect(story.reactionSummary("s1").mine).toEqual(["❤️"]);

    const refresh = story.refresh();
    await settleFetch();
    await story.toggleReaction("s1", "👍");

    resolveSummary({ counts: { "❤️": 1, "🔥": 1 }, reactors: {}, mine: [] });
    await refresh;

    expect(story.reactionSummary("s1").counts).toEqual({ "❤️": 1, "🔥": 1, "👍": 1 });
    expect(story.reactionSummary("s1").mine).toEqual(["❤️", "👍"]);
  });

  test("failed older reaction publish does not roll back a newer toggle", async () => {
    let rejectFirst!: (error: Error) => void;
    const firstPublish = new Promise<void>((_, reject) => {
      rejectFirst = reject;
    });
    let publishCount = 0;
    const client = makeClient([{ id: "s1", body: "story", postedMs: 1 }]);
    client.fetchStoryReactionSummary = mock(() => Promise.resolve({ counts: {}, reactors: {}, mine: [] })) as unknown as MockClient["fetchStoryReactionSummary"];
    client.fetchMyStoryReactions = mock(() => Promise.resolve(null)) as unknown as MockClient["fetchMyStoryReactions"];
    client.publishStoryReactions = mock(() => {
      publishCount += 1;
      return publishCount === 1 ? firstPublish : Promise.resolve();
    }) as unknown as MockClient["publishStoryReactions"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();

    const firstToggle = story.toggleReaction("s1", "🎉");
    await settleFetch();
    const secondToggle = await story.toggleReaction("s1", "👍");
    expect(secondToggle).toBe(true);
    expect(story.reactionSummary("s1").mine).toEqual(["🎉", "👍"]);

    rejectFirst(new Error("older publish failed"));
    expect(await firstToggle).toBe(false);
    expect(story.reactionSummary("s1").mine).toEqual(["🎉", "👍"]);
  });

  test("reaction refresh only preserves locally mutated story reactions", async () => {
    let phase: "initial" | "pending" = "initial";
    let resolveS1!: (summary: StoryReactionSummary) => void;
    let resolveS2!: (summary: StoryReactionSummary) => void;
    const pendingByStory = new Map<string, Promise<StoryReactionSummary>>();
    pendingByStory.set("s1", new Promise((resolve) => {
      resolveS1 = resolve;
    }));
    pendingByStory.set("s2", new Promise((resolve) => {
      resolveS2 = resolve;
    }));
    const client = makeClient([
      { id: "s1", body: "story 1", postedMs: 2 },
      { id: "s2", body: "story 2", postedMs: 1 },
    ]);
    client.fetchStoryReactionSummary = mock((_: string, storyId: string) => {
      if (phase === "initial") {
        return Promise.resolve(storyId === "s2" ? { counts: { "❤️": 1 }, reactors: {}, mine: [] } : { counts: {}, reactors: {}, mine: [] });
      }
      return pendingByStory.get(storyId) ?? Promise.resolve({ counts: {}, reactors: {}, mine: [] });
    }) as unknown as MockClient["fetchStoryReactionSummary"];
    client.fetchMyStoryReactions = mock((_: string, storyId: string) => {
      if (phase === "initial") {
        return Promise.resolve(storyId === "s2"
          ? { jid: "alice@example.com", emojis: ["❤️"], unknownChildrenXml: [] }
          : null);
      }
      return Promise.resolve(storyId === "s2"
        ? { jid: "alice@example.com", emojis: ["🔥"], unknownChildrenXml: [] }
        : null);
    }) as unknown as MockClient["fetchMyStoryReactions"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();
    expect(story.reactionSummary("s2").mine).toEqual(["❤️"]);

    phase = "pending";
    const refresh = story.refresh();
    await settleFetch();
    await story.toggleReaction("s1", "🎉");
    resolveS1({ counts: { "👍": 1 }, reactors: {}, mine: [] });
    resolveS2({ counts: { "🔥": 1 }, reactors: {}, mine: [] });
    await refresh;

    expect(story.reactionSummary("s1").mine).toEqual(["🎉"]);
    expect(story.reactionSummary("s1").counts).toEqual({ "👍": 1, "🎉": 1 });
    expect(story.reactionSummary("s2").mine).toEqual(["🔥"]);
  });

  test("reaction refresh bounds concurrent IQ fetches", async () => {
    let active = 0;
    let maxActive = 0;
    const client = makeClient(
      Array.from({ length: 20 }, (_, index) => ({ id: `s${index}`, body: "story", postedMs: index })),
    );
    client.fetchMyStoryReactions = mock(() => Promise.resolve(null)) as unknown as MockClient["fetchMyStoryReactions"];
    client.fetchStoryReactionSummary = mock(async () => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      await Promise.resolve();
      active -= 1;
      return { counts: {}, reactors: {}, mine: [] };
    }) as unknown as MockClient["fetchStoryReactionSummary"];

    const story = withScope(() =>
      useStories(ref<BrowserXmppClient | null>(client), { authorJids: ref<readonly string[]>([AUTHOR]) }),
    );
    await story.refresh();

    expect(maxActive).toBeLessThanOrEqual(8);
  });
});
