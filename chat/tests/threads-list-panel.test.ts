// Tests for `ThreadsListPanel.vue` — the global Threads view.
//
// Mirrors the test pattern used by `user-profile-drawer.test.ts`:
// a small in-memory model that captures the same fetch + partition
// decisions the component makes, exercised against a stubbed
// `BrowserXmppClient.fetchThreads`.

import { describe, expect, mock, test } from "bun:test";
import type { WasmThreadEntry, WasmThreadsPage } from "../src/lib/xmpp/wasm-types";

interface PanelClient {
  fetchThreads: (opts: { pageSize?: number }) => Promise<WasmThreadsPage>;
}

interface PanelState {
  loading: boolean;
  page: WasmThreadsPage | null;
  error: string | null;
}

interface PanelView {
  unread: WasmThreadEntry[];
  following: WasmThreadEntry[];
  isEmpty: boolean;
  errorText: string | null;
}

/**
 * Mirrors `ThreadsListPanel.vue`'s on-mount flow: call fetchThreads,
 * record the result, then partition into Unread (has_unread) and
 * Following (!has_unread).
 */
function createPanel(client: PanelClient | null) {
  const state: PanelState = {
    loading: true,
    page: null,
    error: null,
  };

  async function mount() {
    if (!client) {
      state.loading = false;
      return;
    }
    try {
      state.page = await client.fetchThreads({ pageSize: 50 });
    } catch (err) {
      state.error = err instanceof Error ? err.message : String(err);
    } finally {
      state.loading = false;
    }
  }

  function view(): PanelView {
    const entries = state.page?.entries ?? [];
    const unread = entries.filter((entry) => entry.has_unread);
    const following = entries.filter((entry) => !entry.has_unread);
    return {
      unread,
      following,
      isEmpty: !state.loading && !state.error && unread.length === 0 && following.length === 0,
      errorText: state.error,
    };
  }

  function openThreadEvents() {
    const captured: Array<{ entry: WasmThreadEntry }> = [];
    function emit(entry: WasmThreadEntry) {
      captured.push({ entry });
    }
    return { emit, captured };
  }

  return { state, mount, view, openThreadEvents };
}

function makeEntry(overrides: Partial<WasmThreadEntry> = {}): WasmThreadEntry {
  return {
    channel: "room@x",
    thread_id: "t",
    last_stanza_id: "S",
    last_activity: new Date().toISOString(),
    unread: 0,
    reply_count: 0,
    has_unread: false,
    ...overrides,
  };
}

describe("ThreadsListPanel", () => {
  test("renders empty state when no entries", async () => {
    const client: PanelClient = {
      fetchThreads: mock(async () => ({
        total: 0,
        unread_threads: 0,
        entries: [],
      })),
    };
    const panel = createPanel(client);
    await panel.mount();
    const view = panel.view();
    expect(view.isEmpty).toBe(true);
    expect(view.unread).toHaveLength(0);
    expect(view.following).toHaveLength(0);
  });

  test("splits unread and following sections", async () => {
    const client: PanelClient = {
      fetchThreads: mock(async () => ({
        total: 3,
        unread_threads: 2,
        entries: [
          makeEntry({ thread_id: "t1", has_unread: true, unread: 2 }),
          makeEntry({ thread_id: "t2", has_unread: false }),
          makeEntry({ thread_id: "t3", has_unread: true, unread: 1 }),
        ],
      })),
    };
    const panel = createPanel(client);
    await panel.mount();
    const view = panel.view();
    expect(view.unread.map((e) => e.thread_id)).toEqual(["t1", "t3"]);
    expect(view.following.map((e) => e.thread_id)).toEqual(["t2"]);
    expect(view.isEmpty).toBe(false);
  });

  test("openThread captures click target", async () => {
    const entry = makeEntry({ thread_id: "click-me", has_unread: true, unread: 1 });
    const client: PanelClient = {
      fetchThreads: mock(async () => ({
        total: 1,
        unread_threads: 1,
        entries: [entry],
      })),
    };
    const panel = createPanel(client);
    await panel.mount();
    const events = panel.openThreadEvents();
    // Simulate a click on the first unread row.
    const target = panel.view().unread[0];
    expect(target).toBeDefined();
    if (target) events.emit(target);
    expect(events.captured).toHaveLength(1);
    expect(events.captured[0]?.entry.thread_id).toBe("click-me");
  });

  test("surfaces fetch errors", async () => {
    const client: PanelClient = {
      fetchThreads: mock(async () => {
        throw new Error("boom");
      }),
    };
    const panel = createPanel(client);
    await panel.mount();
    const view = panel.view();
    expect(view.errorText).toBe("boom");
    expect(view.isEmpty).toBe(false);
  });

  test("no client renders empty without fetching", async () => {
    const panel = createPanel(null);
    await panel.mount();
    expect(panel.state.loading).toBe(false);
    expect(panel.state.page).toBeNull();
    expect(panel.view().isEmpty).toBe(true);
  });
});
