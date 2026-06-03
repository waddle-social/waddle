import { describe, expect, mock, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useThreadsListPanelState, type ThreadsViewClient } from "../src/lib/threads-view-state";
import type { ChannelSummary } from "../src/lib/chat-types";
import type { WasmThreadEntry, WasmThreadsPage } from "../src/lib/xmpp/wasm-types";

function makeEntry(overrides: Partial<WasmThreadEntry> = {}): WasmThreadEntry {
  return {
    channel: "room@x",
    thread_id: "t",
    last_stanza_id: "S",
    last_activity: new Date("2026-06-02T10:00:00.000Z").toISOString(),
    unread: 0,
    reply_count: 0,
    has_unread: false,
    ...overrides,
  };
}

function page(entries: WasmThreadEntry[], nextCursor?: string): WasmThreadsPage {
  return {
    total: entries.length,
    unread_threads: entries.filter((entry) => entry.has_unread).length,
    entries,
    ...(nextCursor ? { next_cursor: nextCursor } : {}),
  };
}

function flushWatchers() {
  return nextTick().then(() => Promise.resolve());
}

function createPanel(options: {
  client?: ThreadsViewClient | null;
  channels?: ChannelSummary[];
  search?: string;
} = {}) {
  const scope = effectScope();
  const clientRef = ref<ThreadsViewClient | null>(options.client ?? null);
  const channelsRef = ref<ChannelSummary[]>(options.channels ?? []);
  const replacedSearch: string[] = [];
  let currentSearch = options.search ?? "";
  const panel = scope.run(() =>
    useThreadsListPanelState({
      xmppClient: clientRef,
      channels: channelsRef,
      now: () => new Date("2026-06-02T12:00:00.000Z"),
      urlState: {
        readSearch: () => currentSearch,
        replaceSearch: (encoded) => {
          currentSearch = encoded ? `?${encoded}` : "";
          replacedSearch.push(encoded);
        },
      },
    }),
  );
  if (!panel) throw new Error("failed to create panel state");
  return { channelsRef, clientRef, currentSearch: () => currentSearch, panel, replacedSearch, scope };
}

describe("ThreadsListPanel state", () => {
  test("fetches the default protocol-backed page", async () => {
    const client: ThreadsViewClient = {
      fetchThreads: mock(async () => page([])),
      markInboxRead: mock(async () => {}),
    };
    const { panel, scope } = createPanel({ client });

    await panel.fetchThreadsPage(false);

    expect(client.fetchThreads).toHaveBeenCalledTimes(1);
    // Default view is now Unread over the last 7 days.
    expect(client.fetchThreads.mock.calls[0]?.[0]).toEqual({
      pageSize: 50,
      status: "unread",
      activeSince: "2026-05-26T00:00:00.000Z",
      sort: "recent",
    });
    expect(panel.hasEntries.value).toBe(false);
    scope.stop();
  });

  test("filter refs drive fetch options, URL state, and cursor reset", async () => {
    const client: ThreadsViewClient = {
      fetchThreads: mock(async (opts) =>
        page([makeEntry({ thread_id: opts?.afterCursor ?? "first" })], "cursor-1"),
      ),
      markInboxRead: mock(async () => {}),
    };
    const { panel, replacedSearch, scope } = createPanel({
      client,
      channels: [{ id: "chat", name: "Chat", jid: "chat@muc.waddle.chat" }],
    });

    await panel.fetchThreadsPage(false);
    await panel.fetchThreadsPage(true);

    panel.status.value = "unread";
    panel.active.value = "14d";
    panel.channel.value = "chat";
    panel.search.value = " notifications ";
    panel.sort.value = "replies";
    await flushWatchers();

    expect(client.fetchThreads.mock.calls[1]?.[0]).toMatchObject({
      afterCursor: "cursor-1",
    });
    expect(client.fetchThreads.mock.calls.at(-1)?.[0]).toEqual({
      pageSize: 50,
      status: "unread",
      activeSince: "2026-05-19T00:00:00.000Z",
      channel: "chat@muc.waddle.chat",
      search: "notifications",
      sort: "replies",
    });
    // `unread` is the default status now, so it drops out of the URL;
    // the non-default 14d window and the rest still round-trip.
    expect(replacedSearch.at(-1)).toBe(
      "active=14d&channel=chat&q=notifications&sort=replies",
    );
    scope.stop();
  });

  test("restored channel filters wait for channel metadata before fetching", async () => {
    const client: ThreadsViewClient = {
      fetchThreads: mock(async () => page([makeEntry({ thread_id: "chat-thread" })])),
      markInboxRead: mock(async () => {}),
    };
    const { channelsRef, panel, scope } = createPanel({
      client,
      search: "?channel=chat",
    });

    await panel.fetchThreadsPage(false);
    expect(client.fetchThreads).not.toHaveBeenCalled();
    expect(panel.hasEntries.value).toBe(false);
    expect(panel.channelFilterMessage.value).toBe(
      "Waiting for channel metadata before filtering threads.",
    );
    expect(panel.channelFilterOptionLabel.value).toBe("Loading selected channel");

    channelsRef.value = [{ id: "chat", name: "Chat", jid: "chat@muc.waddle.chat" }];
    await flushWatchers();

    expect(client.fetchThreads).toHaveBeenCalledTimes(1);
    expect(client.fetchThreads.mock.calls[0]?.[0]).toMatchObject({
      channel: "chat@muc.waddle.chat",
    });
    expect(panel.channelFilterMessage.value).toBe("");
    expect(panel.channelFilterOptionLabel.value).toBe("");
    scope.stop();
  });

  test("unresolved restored channel filters expose an unavailable-channel state", async () => {
    const client: ThreadsViewClient = {
      fetchThreads: mock(async () => page([])),
      markInboxRead: mock(async () => {}),
    };
    const { panel, scope } = createPanel({
      client,
      channels: [{ id: "chat", name: "Chat", jid: "chat@muc.waddle.chat" }],
      search: "?channel=missing",
    });

    await panel.fetchThreadsPage(false);

    expect(client.fetchThreads).not.toHaveBeenCalled();
    expect(panel.hasEntries.value).toBe(false);
    expect(panel.channelFilterMessage.value).toBe("Selected channel is no longer available.");
    expect(panel.channelFilterOptionLabel.value).toBe("Selected channel unavailable");
    scope.stop();
  });

  test("sections preserve server order for all results and specialize filtered views", async () => {
    const following = makeEntry({ thread_id: "f1", has_unread: false });
    const unread = makeEntry({ thread_id: "u1", has_unread: true, unread: 2 });
    const client: ThreadsViewClient = {
      fetchThreads: mock(async (opts) => {
        if (opts?.status === "unread") return page([unread]);
        if (opts?.status === "following") return page([following]);
        return page([following, unread]);
      }),
      markInboxRead: mock(async () => {}),
    };
    const { panel, scope } = createPanel({ client });

    // Default view is Unread now; this case exercises the All view, so
    // widen the status filter explicitly before fetching.
    panel.status.value = "all";
    await panel.fetchThreadsPage(false);
    expect(panel.sections.value).toHaveLength(1);
    expect(panel.sections.value[0]?.label).toBe("All");
    expect(panel.sections.value[0]?.entries.map((entry) => entry.thread_id)).toEqual(["f1", "u1"]);

    panel.status.value = "unread";
    await flushWatchers();
    expect(panel.sections.value[0]?.label).toBe("Unread");
    expect(panel.sections.value[0]?.entries.map((entry) => entry.thread_id)).toEqual(["u1"]);

    panel.status.value = "following";
    await flushWatchers();
    expect(panel.sections.value[0]?.label).toBe("Following");
    expect(panel.sections.value[0]?.entries.map((entry) => entry.thread_id)).toEqual(["f1"]);
    scope.stop();
  });

  test("mark read calls the thread-aware inbox API and refetches the first page", async () => {
    const entry = makeEntry({
      channel: "chat@muc.waddle.chat",
      thread_id: "thread-1",
      has_unread: true,
      unread: 3,
    });
    const client: ThreadsViewClient = {
      fetchThreads: mock(async () => page([entry])),
      markInboxRead: mock(async () => {}),
    };
    const { panel, scope } = createPanel({ client });

    await panel.fetchThreadsPage(false);
    await panel.markThreadRead(entry);

    expect(client.markInboxRead).toHaveBeenCalledWith("chat@muc.waddle.chat", "thread-1");
    expect(client.fetchThreads).toHaveBeenCalledTimes(2);
    expect(client.fetchThreads.mock.calls[1]?.[0]).toEqual({
      pageSize: 50,
      status: "unread",
      activeSince: "2026-05-26T00:00:00.000Z",
      sort: "recent",
    });
    scope.stop();
  });

  test("surfaces fetch errors", async () => {
    const client: ThreadsViewClient = {
      fetchThreads: mock(async () => {
        throw new Error("boom");
      }),
      markInboxRead: mock(async () => {}),
    };
    const { panel, scope } = createPanel({ client });

    await panel.fetchThreadsPage(false);

    expect(panel.error.value).toBe("boom");
    expect(panel.hasEntries.value).toBe(false);
    scope.stop();
  });

  test("no client renders empty without fetching", async () => {
    const { panel, scope } = createPanel();

    await panel.fetchThreadsPage(false);

    expect(panel.loading.value).toBe(false);
    expect(panel.hasEntries.value).toBe(false);
    scope.stop();
  });
});
