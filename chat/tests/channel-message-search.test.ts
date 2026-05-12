// Direct unit tests for useChannelMessageSearch covering XEP-0313 query
// dispatch, the empty-query short-circuit, error surfacing, and the
// request-token race-id guard that drops stale server responses.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelMessageSearch } from "../src/channels/message-search";
import type { BrowserXmppClient, MessageSearchResult } from "../src/lib/xmpp-client";

function makeResult(id: string, body: string): MessageSearchResult {
  return { id, body } as unknown as MessageSearchResult;
}

function harness(opts: { client?: BrowserXmppClient; channelId?: string | null } = {}) {
  const xmppClient = ref<BrowserXmppClient | null>(
    opts.client ?? ({ searchMessages: mock(async () => []) } as unknown as BrowserXmppClient),
  );
  const activeSpaceId = ref("space");
  const activeChannelId = ref<string | null>(
    opts.channelId === undefined ? "general" : opts.channelId,
  );
  const actionError = ref("");
  const clearActionError = mock(() => { actionError.value = ""; });

  const search = useChannelMessageSearch({
    xmppClient,
    activeSpaceId,
    activeChannelId,
    actionError,
    clearActionError,
    normalizeError: (e) => String(e),
  });

  return { xmppClient, activeChannelId, actionError, search };
}

describe("useChannelMessageSearch — happy path", () => {
  test("forwards the trimmed query to the client and stores results", async () => {
    const searchMessages = mock(async () => [makeResult("m1", "match")]);
    const client = { searchMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });
    await h.search.searchMessages("  alice  ");
    expect(searchMessages.mock.calls[0]?.[2]).toBe("alice");
    expect(h.search.searchQuery.value).toBe("alice");
    expect(h.search.searchResults.value).toHaveLength(1);
    expect(h.search.isSearching.value).toBe(false);
  });
});

describe("useChannelMessageSearch — guards", () => {
  test("no client / no channel: silent no-op", async () => {
    const searchMessages = mock(async () => []);
    const client = { searchMessages } as unknown as BrowserXmppClient;
    const h = harness({ client, channelId: null });
    await h.search.searchMessages("anything");
    expect(searchMessages).not.toHaveBeenCalled();
  });

  test("empty / whitespace-only query: clears results, no send", async () => {
    const searchMessages = mock(async () => []);
    const client = { searchMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });
    h.search.searchResults.value = [makeResult("stale", "")];
    await h.search.searchMessages("   ");
    expect(searchMessages).not.toHaveBeenCalled();
    expect(h.search.searchResults.value).toEqual([]);
    expect(h.search.isSearching.value).toBe(false);
  });
});

describe("useChannelMessageSearch — race-id stale-response guard", () => {
  test("a slow response from an earlier query never overwrites the later query's results", async () => {
    // Two in-flight queries: the first resolves after the second has
    // already started. The race-id guard inside searchMessages must drop
    // the first's payload so it can't clobber the second's.
    let resolveFirst: (v: MessageSearchResult[]) => void = () => {};
    let resolveSecond: (v: MessageSearchResult[]) => void = () => {};
    let callCount = 0;
    const searchMessages = mock(() => {
      callCount++;
      if (callCount === 1) {
        return new Promise<MessageSearchResult[]>((resolve) => { resolveFirst = resolve; });
      }
      return new Promise<MessageSearchResult[]>((resolve) => { resolveSecond = resolve; });
    });
    const client = { searchMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });

    const p1 = h.search.searchMessages("first");
    const p2 = h.search.searchMessages("second");
    // Resolve the SECOND query first — that wins.
    resolveSecond([makeResult("second-result", "")]);
    // Then resolve the FIRST query — its result must be dropped, not
    // overwrite "second-result".
    resolveFirst([makeResult("first-result", "")]);
    await Promise.all([p1, p2]);

    expect(h.search.searchResults.value).toHaveLength(1);
    expect(h.search.searchResults.value[0]?.id).toBe("second-result");
    // searchQuery still reflects whatever was set most recently.
    expect(h.search.searchQuery.value).toBe("second");
  });

  test("reset() bumps the request token so a still-in-flight query's response is dropped", async () => {
    let resolveSlow: (v: MessageSearchResult[]) => void = () => {};
    const searchMessages = mock(
      () => new Promise<MessageSearchResult[]>((r) => { resolveSlow = r; }),
    );
    const client = { searchMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });

    const p = h.search.searchMessages("alice");
    h.search.reset();
    resolveSlow([makeResult("late", "")]);
    await p;

    expect(h.search.searchResults.value).toEqual([]);
    expect(h.search.isSearching.value).toBe(false);
    expect(h.search.searchQuery.value).toBe("");
  });
});

describe("useChannelMessageSearch — error path", () => {
  test("search throws: sets actionError, clears results, resets isSearching", async () => {
    const searchMessages = mock(async () => { throw new Error("server angry"); });
    const client = { searchMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });
    await h.search.searchMessages("alice");
    expect(h.search.searchResults.value).toEqual([]);
    expect(h.search.isSearching.value).toBe(false);
    expect(h.actionError.value).toContain("server angry");
  });
});

describe("useChannelMessageSearch.clearSearch", () => {
  test("wipes query/results and resets isSearching", () => {
    const h = harness();
    h.search.searchQuery.value = "alice";
    h.search.searchResults.value = [makeResult("x", "")];
    h.search.isSearching.value = true;
    h.search.clearSearch();
    expect(h.search.searchQuery.value).toBe("");
    expect(h.search.searchResults.value).toEqual([]);
    expect(h.search.isSearching.value).toBe(false);
  });
});
