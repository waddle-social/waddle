import { describe, expect, test } from "bun:test";
import { ref } from "vue";
import { useMessageThreads } from "../src/channels/threads";
import type { TimelineMessage } from "../src/lib/chat-ui";

function makeMessage(partial: Partial<TimelineMessage> & { id: string; createdAt: string }): TimelineMessage {
  return {
    author: "alice",
    body: "",
    isSelf: false,
    ...partial,
  };
}

describe("useMessageThreads", () => {
  test("returns an empty index when there are no threaded messages", () => {
    const messages = ref<TimelineMessage[]>([
      makeMessage({ id: "a", createdAt: "2026-01-01T00:00:00Z", body: "hi" }),
      makeMessage({ id: "b", createdAt: "2026-01-01T00:01:00Z", body: "yo" }),
    ]);
    const { index } = useMessageThreads(messages);
    expect(index.value.size).toBe(0);
  });

  test("resolves the root by matching threadId against a loaded message id", () => {
    // Waddle's root messages don't carry <thread>; replies set threadId to
    // the root's id. We resolve root via an id lookup, not id===threadId.
    const messages = ref<TimelineMessage[]>([
      makeMessage({ id: "root", createdAt: "2026-01-01T00:00:00Z", body: "root msg" }),
      makeMessage({ id: "c1", threadId: "root", createdAt: "2026-01-01T00:01:00Z", body: "first reply" }),
      makeMessage({ id: "c2", threadId: "root", createdAt: "2026-01-01T00:02:00Z", body: "second reply" }),
    ]);
    const { index, hasChildren, rootOf } = useMessageThreads(messages);
    const entry = index.value.get("root");
    expect(entry).toBeTruthy();
    expect(entry!.root?.id).toBe("root");
    expect(entry!.directChildren.map((m) => m.id)).toEqual(["c1", "c2"]);
    expect(entry!.count).toBe(2);
    expect(entry!.lastTs).toBe("2026-01-01T00:02:00Z");
    expect(hasChildren("root")).toBe(true);
    expect(rootOf(messages.value[1])?.id).toBe("root");
  });

  test("marks threads as orphan when the root isn't in the loaded window", () => {
    // Waddle reply whose root scrolled off: the reply has threadId pointing
    // at the missing root id and a XEP-0461 replyTo pointing at the same id.
    // The disambiguation rule must NOT promote the child to root just because
    // it is the only loaded member of the thread.
    const messages = ref<TimelineMessage[]>([
      makeMessage({
        id: "c1",
        threadId: "missing-root",
        replyTo: { id: "missing-root", author: "alice" },
        createdAt: "2026-01-01T00:01:00Z",
        body: "orphan reply",
      }),
    ]);
    const { index } = useMessageThreads(messages);
    const entry = index.value.get("missing-root");
    expect(entry).toBeTruthy();
    expect(entry!.root).toBeNull();
    expect(entry!.directChildren.length).toBe(1);
    expect(entry!.count).toBe(1);
  });

  test("rolls nested sub-thread descendants into ancestor counts", () => {
    // Outer thread rooted at `root`; `c1` is a direct reply whose id is
    // reused as the sub-thread id. Messages inside the sub-thread carry
    // threadId=c1 / parentThreadId=root.
    const messages = ref<TimelineMessage[]>([
      makeMessage({ id: "root", createdAt: "2026-01-01T00:00:00Z", body: "root" }),
      makeMessage({ id: "c1", threadId: "root", createdAt: "2026-01-01T00:01:00Z", body: "reply" }),
      makeMessage({ id: "sub1", threadId: "c1", parentThreadId: "root", createdAt: "2026-01-01T00:02:00Z", body: "nested" }),
      makeMessage({ id: "sub2", threadId: "c1", parentThreadId: "root", createdAt: "2026-01-01T00:03:00Z", body: "nested 2" }),
    ]);
    const { index } = useMessageThreads(messages);
    const rootEntry = index.value.get("root");
    const subEntry = index.value.get("c1");
    expect(subEntry).toBeTruthy();
    // c1 itself belongs to the outer thread's group, so the sub-thread's
    // root is resolved via byId (c1 is loaded) rather than being an orphan.
    expect(subEntry!.root?.id).toBe("c1");
    expect(subEntry!.count).toBe(2);
    expect(rootEntry).toBeTruthy();
    // Ancestor count includes its own direct child + nested descendants.
    expect(rootEntry!.count).toBe(3);
    expect(rootEntry!.lastTs).toBe("2026-01-01T00:03:00Z");
  });

  test("getEntry returns undefined for unknown ids", () => {
    const messages = ref<TimelineMessage[]>([]);
    const { getEntry } = useMessageThreads(messages);
    expect(getEntry(undefined)).toBeUndefined();
    expect(getEntry("nope")).toBeUndefined();
  });

  test("resolveEntry synthesises an empty entry for a freshly-started sub-thread", () => {
    // User clicks "Start sub-thread" on a reply (`c1`) before typing. The
    // sub-thread has zero messages, so there's no index entry — but the
    // panel still needs to render the root + composer instead of "Loading…".
    const messages = ref<TimelineMessage[]>([
      makeMessage({ id: "root", threadId: "root", createdAt: "2026-01-01T00:00:00Z", body: "root msg" }),
      makeMessage({ id: "c1", threadId: "root", createdAt: "2026-01-01T00:01:00Z", body: "reply" }),
    ]);
    const { getEntry, resolveEntry } = useMessageThreads(messages);
    expect(getEntry("c1")).toBeUndefined();
    const entry = resolveEntry("c1");
    expect(entry).toBeTruthy();
    expect(entry!.root?.id).toBe("c1");
    expect(entry!.directChildren).toEqual([]);
    expect(entry!.count).toBe(0);
  });

  test("resolveEntry returns undefined when the id doesn't match any loaded message", () => {
    const messages = ref<TimelineMessage[]>([]);
    const { resolveEntry } = useMessageThreads(messages);
    expect(resolveEntry("nope")).toBeUndefined();
    expect(resolveEntry(undefined)).toBeUndefined();
  });

  test("XEP-0201: resolves the root by chronology when the root id is unrelated to the thread id", () => {
    // Standard XEP-0201: the thread element carries an arbitrary identifier
    // that is decoupled from any message id. The root MUST still be the
    // earliest message bearing the thread id; the legacy id===threadId
    // shortcut does not apply here.
    const messages = ref<TimelineMessage[]>([
      makeMessage({
        id: "msg-1",
        threadId: "topic-roadmap",
        createdAt: "2026-01-01T00:00:00Z",
        body: "kicking off the roadmap thread",
      }),
      makeMessage({
        id: "msg-2",
        threadId: "topic-roadmap",
        createdAt: "2026-01-01T00:05:00Z",
        body: "agree, let's split it",
      }),
    ]);
    const { index, rootOf } = useMessageThreads(messages);
    const entry = index.value.get("topic-roadmap");
    expect(entry).toBeTruthy();
    expect(entry!.root?.id).toBe("msg-1");
    expect(entry!.directChildren.map((m) => m.id)).toEqual(["msg-2"]);
    expect(entry!.count).toBe(1);
    expect(rootOf(messages.value[1])?.id).toBe("msg-1");
  });

  test("DM timeline messages produce thread entries from XEP-0201 ids", () => {
    const messages = ref<TimelineMessage[]>([
      makeMessage({
        id: "dm-root-1",
        author: "Bob",
        authorJid: "bob@example.com/phone",
        createdAt: "2026-01-01T00:00:00Z",
        body: "want to split this out?",
      }),
      makeMessage({
        id: "dm-reply-1",
        author: "alice",
        authorJid: "alice@example.com/desktop",
        threadId: "dm-root-1",
        replyTo: { id: "dm-root-1", author: "Bob", preview: "want to split this out?" },
        createdAt: "2026-01-01T00:01:00Z",
        body: "yes",
        isSelf: true,
      }),
    ]);

    const { index, resolveEntry } = useMessageThreads(messages);
    const entry = index.value.get("dm-root-1");

    expect(entry).toBeTruthy();
    expect(resolveEntry("dm-root-1")?.root?.authorJid).toBe("bob@example.com/phone");
    expect(entry!.directChildren.map((message) => message.id)).toEqual(["dm-reply-1"]);
    expect(entry!.count).toBe(1);
  });
});
