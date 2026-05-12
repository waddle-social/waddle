// Direct unit tests for useDmMessageSearch — mirror of the channel side
// minus searchQuery (the DM UI keeps the query in the input ref directly).

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useDmMessageSearch } from "../src/dms/message-search";
import type { BrowserXmppClient, MessageSearchResult } from "../src/lib/xmpp-client";

function makeResult(id: string): MessageSearchResult {
  return { id, body: "" } as unknown as MessageSearchResult;
}

function harness(opts: { client?: BrowserXmppClient; peerJid?: string | null } = {}) {
  const xmppClient = ref<BrowserXmppClient | null>(
    opts.client ?? ({ searchDmMessages: mock(async () => []) } as unknown as BrowserXmppClient),
  );
  const activePeerJid = ref<string | null>(
    opts.peerJid === undefined ? "bob@example.com" : opts.peerJid,
  );
  const actionError = ref("");
  const clearActionError = mock(() => { actionError.value = ""; });

  const search = useDmMessageSearch({
    xmppClient,
    activePeerJid,
    actionError,
    clearActionError,
    normalizeError: (e) => String(e),
  });

  return { xmppClient, activePeerJid, actionError, search };
}

describe("useDmMessageSearch — happy path + guards", () => {
  test("forwards trimmed query to searchDmMessages and stores results", async () => {
    const searchDmMessages = mock(async () => [makeResult("dm1")]);
    const client = { searchDmMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });
    await h.search.searchMessages("  hi  ");
    expect(searchDmMessages.mock.calls[0]?.[1]).toBe("hi");
    expect(h.search.searchResults.value).toHaveLength(1);
  });

  test("no client / no peer: silent no-op", async () => {
    const searchDmMessages = mock(async () => []);
    const client = { searchDmMessages } as unknown as BrowserXmppClient;
    const h = harness({ client, peerJid: null });
    await h.search.searchMessages("hi");
    expect(searchDmMessages).not.toHaveBeenCalled();
  });

  test("empty query clears results and skips the send", async () => {
    const searchDmMessages = mock(async () => []);
    const client = { searchDmMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });
    h.search.searchResults.value = [makeResult("stale")];
    await h.search.searchMessages("");
    expect(searchDmMessages).not.toHaveBeenCalled();
    expect(h.search.searchResults.value).toEqual([]);
  });
});

describe("useDmMessageSearch — race-id stale-response guard", () => {
  test("peer switch mid-query drops the in-flight response", async () => {
    let resolveSlow: (v: MessageSearchResult[]) => void = () => {};
    const searchDmMessages = mock(
      () => new Promise<MessageSearchResult[]>((r) => { resolveSlow = r; }),
    );
    const client = { searchDmMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });

    const p = h.search.searchMessages("hi bob");
    // Peer changed mid-flight — the race-id guard's peerJid check kicks in.
    h.activePeerJid.value = "carol@example.com";
    resolveSlow([makeResult("late-for-bob")]);
    await p;
    expect(h.search.searchResults.value).toEqual([]);
  });
});

describe("useDmMessageSearch — error path + clearSearch", () => {
  test("search throws → actionError set, results cleared", async () => {
    const searchDmMessages = mock(async () => { throw new Error("offline"); });
    const client = { searchDmMessages } as unknown as BrowserXmppClient;
    const h = harness({ client });
    await h.search.searchMessages("hi");
    expect(h.search.searchResults.value).toEqual([]);
    expect(h.actionError.value).toContain("offline");
    expect(h.search.isSearching.value).toBe(false);
  });

  test("clearSearch wipes results + isSearching", () => {
    const h = harness();
    h.search.searchResults.value = [makeResult("x")];
    h.search.isSearching.value = true;
    h.search.clearSearch();
    expect(h.search.searchResults.value).toEqual([]);
    expect(h.search.isSearching.value).toBe(false);
  });
});
