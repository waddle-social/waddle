import { describe, expect, test } from "bun:test";
import { ref } from "vue";
import { useThreads } from "../src/composables/useThreads";
import type { TimelineMessage } from "../src/lib/chat-ui";

function makeMessage(partial: Partial<TimelineMessage> & { id: string; createdAt: string }): TimelineMessage {
  return {
    author: "alice",
    body: "",
    isSelf: false,
    ...partial,
  };
}

describe("useThreads", () => {
  test("returns an empty index when there are no threaded messages", () => {
    const messages = ref<TimelineMessage[]>([
      makeMessage({ id: "a", createdAt: "2026-01-01T00:00:00Z", body: "hi" }),
      makeMessage({ id: "b", createdAt: "2026-01-01T00:01:00Z", body: "yo" }),
    ]);
    const { index } = useThreads(messages);
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
    const { index, hasChildren, rootOf } = useThreads(messages);
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
    const messages = ref<TimelineMessage[]>([
      makeMessage({ id: "c1", threadId: "missing-root", createdAt: "2026-01-01T00:01:00Z", body: "orphan reply" }),
    ]);
    const { index } = useThreads(messages);
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
    const { index } = useThreads(messages);
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
    const { getEntry } = useThreads(messages);
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
    const { getEntry, resolveEntry } = useThreads(messages);
    expect(getEntry("c1")).toBeUndefined();
    const entry = resolveEntry("c1");
    expect(entry).toBeTruthy();
    expect(entry!.root?.id).toBe("c1");
    expect(entry!.directChildren).toEqual([]);
    expect(entry!.count).toBe(0);
  });

  test("resolveEntry returns undefined when the id doesn't match any loaded message", () => {
    const messages = ref<TimelineMessage[]>([]);
    const { resolveEntry } = useThreads(messages);
    expect(resolveEntry("nope")).toBeUndefined();
    expect(resolveEntry(undefined)).toBeUndefined();
  });
});
