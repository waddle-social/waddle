import { describe, expect, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import type { UserSearchResult } from "../src/lib/chat-types";
import { useMentionDirectory } from "../src/shell/controllers/use-mention-directory";

const DEBOUNCE_MS = 5;
const settle = () => new Promise((resolve) => setTimeout(resolve, DEBOUNCE_MS * 4));

function user(username: string): UserSearchResult {
  const jid = `${username}@example.com`;
  return { id: jid, jid, username, display_name: null };
}

function harness(searchUsers: (query: string) => Promise<UserSearchResult[]>, room: string | null = "room-1") {
  const activeRoomId = ref<string | null>(room);
  const scope = effectScope();
  const directory = scope.run(() => useMentionDirectory({ searchUsers, activeRoomId, debounceMs: DEBOUNCE_MS }))!;
  return { directory, activeRoomId, stop: () => scope.stop() };
}

describe("useMentionDirectory", () => {
  test("debounces typing into one search and exposes the latest query's users", async () => {
    const calls: string[] = [];
    const { directory, stop } = harness(async (query) => {
      calls.push(query);
      return [user("dave")];
    });
    directory.search("d");
    directory.search("da");
    await settle();
    expect(calls).toEqual(["da"]);
    expect(directory.results.value).toEqual([{ username: "dave", jid: "dave@example.com" }]);
    expect(directory.seenUsers.value).toEqual([{ username: "dave", jid: "dave@example.com" }]);
    stop();
  });

  test("ignores a stale response when a newer query has been issued", async () => {
    const pending = new Map<string, (users: UserSearchResult[]) => void>();
    const { directory, stop } = harness((query) => new Promise((resolve) => pending.set(query, resolve)));
    directory.search("da");
    await settle();
    directory.search("dav");
    await settle();
    pending.get("dav")!([user("dave")]);
    await settle();
    pending.get("da")!([user("dana")]);
    await settle();
    expect(directory.results.value.map((u) => u.username)).toEqual(["dave"]);
    expect(directory.seenUsers.value.map((u) => u.username)).toEqual(["dave"]);
    stop();
  });

  test("caches per query and keeps every user seen in the room", async () => {
    const calls: string[] = [];
    const { directory, stop } = harness(async (query) => {
      calls.push(query);
      return query === "da" ? [user("dave")] : [user("erin")];
    });
    directory.search("da");
    await settle();
    directory.search("er");
    await settle();
    directory.search("da");
    await settle();
    expect(calls).toEqual(["da", "er"]);
    expect(directory.results.value.map((u) => u.username)).toEqual(["dave"]);
    expect(directory.seenUsers.value.map((u) => u.username)).toEqual(["dave", "erin"]);
    directory.search("");
    expect(directory.results.value).toEqual([]);
    stop();
  });

  test("falls back silently on search errors", async () => {
    const { directory, stop } = harness(async () => {
      throw new Error("service-unavailable");
    });
    directory.search("da");
    await settle();
    expect(directory.results.value).toEqual([]);
    stop();
  });

  test("never searches outside rooms and resets on room change", async () => {
    const calls: string[] = [];
    const { directory, activeRoomId, stop } = harness(async (query) => {
      calls.push(query);
      return [user("dave")];
    }, null);
    directory.search("da");
    await settle();
    expect(calls).toEqual([]);

    activeRoomId.value = "room-1";
    await nextTick();
    directory.search("da");
    await settle();
    expect(directory.seenUsers.value).toHaveLength(1);

    activeRoomId.value = "room-2";
    await nextTick();
    expect(directory.results.value).toEqual([]);
    expect(directory.seenUsers.value).toEqual([]);
    stop();
  });
});
