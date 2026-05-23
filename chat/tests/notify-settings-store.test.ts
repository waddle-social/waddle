// Pin the chat-side XEP-0492 notification-settings store (#532).
//
// The store hydrates from XEP-0402 bookmarks fetched via the WASM
// client, falls back to the XEP-0492 §3 default when no bookmark
// covers a conversation, and propagates `setMode` results into the
// cache so the UI doesn't need a follow-up fetch.

import { describe, expect, test } from "bun:test";
import {
  createNotifySettingsStore,
  effectiveNotifyMode,
  nextMenuIndex,
  resolveDefaultNotifyMode,
} from "../src/lib/notify-settings";
import type {
  BrowserXmppClient,
  NotifyMode,
  SetRoomNotificationModeOutcome,
  UserBookmarkItem,
} from "../src/lib/xmpp-client";

class FakeXmppClient {
  constructor(
    private bookmarks: UserBookmarkItem[],
    public published: { roomJid: string; mode: NotifyMode; name?: string }[] = [],
  ) {}

  async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
    return [...this.bookmarks];
  }

  async setRoomNotificationMode(opts: {
    roomJid: string;
    mode: NotifyMode;
    name?: string;
  }): Promise<SetRoomNotificationModeOutcome> {
    this.published.push(opts);
    const updated: UserBookmarkItem = {
      jid: opts.roomJid,
      name: opts.name ?? null,
      autojoin: false,
      notifyMode: opts.mode,
    };
    const idx = this.bookmarks.findIndex((b) => b.jid === opts.roomJid);
    if (idx >= 0) this.bookmarks[idx] = { ...this.bookmarks[idx], notifyMode: opts.mode };
    else this.bookmarks.push(updated);
    return { kind: "ok", item: updated };
  }
}

function asClient(fake: FakeXmppClient): BrowserXmppClient {
  return fake as unknown as BrowserXmppClient;
}

describe("resolveDefaultNotifyMode", () => {
  test("returns always for direct chats and private groups (XEP-0492 §3)", () => {
    expect(resolveDefaultNotifyMode("direct-chat")).toBe("always");
    expect(resolveDefaultNotifyMode("private-group")).toBe("always");
  });

  test("returns on-mention for public groups (XEP-0492 §3)", () => {
    expect(resolveDefaultNotifyMode("public-group")).toBe("on-mention");
  });
});

describe("effectiveNotifyMode", () => {
  test("uses the stored mode when bookmark carries one", () => {
    const bookmark: UserBookmarkItem = {
      jid: "room@example.com",
      name: null,
      autojoin: false,
      notifyMode: "never",
    };
    expect(effectiveNotifyMode(bookmark, "public-group")).toBe("never");
  });

  test("falls back to XEP-0492 §3 default when bookmark has no mode", () => {
    const bookmark: UserBookmarkItem = {
      jid: "room@example.com",
      name: null,
      autojoin: false,
      notifyMode: null,
    };
    expect(effectiveNotifyMode(bookmark, "public-group")).toBe("on-mention");
    expect(effectiveNotifyMode(bookmark, "private-group")).toBe("always");
  });

  test("falls back to default when no bookmark exists at all", () => {
    expect(effectiveNotifyMode(undefined, "private-group")).toBe("always");
    expect(effectiveNotifyMode(undefined, "public-group")).toBe("on-mention");
  });
});

describe("nextMenuIndex (WAI-ARIA radio menu nav)", () => {
  test("ArrowDown advances and wraps", () => {
    expect(nextMenuIndex(0, 1, 3)).toBe(1);
    expect(nextMenuIndex(2, 1, 3)).toBe(0);
  });

  test("ArrowUp goes back and wraps", () => {
    expect(nextMenuIndex(1, -1, 3)).toBe(0);
    expect(nextMenuIndex(0, -1, 3)).toBe(2);
  });

  test("with no focused item, ArrowDown lands on first, ArrowUp on last", () => {
    expect(nextMenuIndex(-1, 1, 3)).toBe(0);
    expect(nextMenuIndex(-1, -1, 3)).toBe(2);
  });
});

describe("hydrate preserves cache across reconnects (no flicker)", () => {
  test("subsequent hydrate replaces atomically — cache is not cleared mid-flight", async () => {
    const store = createNotifySettingsStore();
    // Initial hydrate.
    await store.hydrate(asClient(new FakeXmppClient([
      { jid: "a@example.com", name: "A", autojoin: false, notifyMode: "never" },
    ])));
    expect(store.bookmarks.value["a@example.com"].notifyMode).toBe("never");

    // Start a second hydrate; the cache must NOT clear before the
    // fetch resolves (P2 round 7: icon flicker on reconnect).
    let resolveFetch: (items: UserBookmarkItem[]) => void = () => {};
    const slowClient = {
      fetchUserBookmarks: () =>
        new Promise<UserBookmarkItem[]>((resolve) => {
          resolveFetch = resolve;
        }),
      setRoomNotificationMode: async (): Promise<SetRoomNotificationModeOutcome> => ({
        kind: "error",
      }),
    };
    const pending = store.hydrate(slowClient as unknown as BrowserXmppClient);
    expect(store.hydrating.value).toBe(true);
    // Cache still holds the previous entry while the slow fetch is
    // in flight.
    expect(store.bookmarks.value["a@example.com"].notifyMode).toBe("never");

    resolveFetch([
      { jid: "a@example.com", name: "A", autojoin: false, notifyMode: "always" },
      { jid: "b@example.com", name: "B", autojoin: false, notifyMode: "never" },
    ]);
    await pending;
    expect(store.hydrating.value).toBe(false);
    expect(store.bookmarks.value["a@example.com"].notifyMode).toBe("always");
    expect(store.bookmarks.value["b@example.com"].notifyMode).toBe("never");
  });
});

describe("createNotifySettingsStore", () => {
  test("hydrate populates the cache from fetchUserBookmarks", async () => {
    const store = createNotifySettingsStore();
    const fake = new FakeXmppClient([
      { jid: "a@example.com", name: "A", autojoin: true, notifyMode: "never" },
      { jid: "b@example.com", name: "B", autojoin: false, notifyMode: null },
    ]);

    expect(store.hydrating.value).toBe(false);
    const promise = store.hydrate(asClient(fake));
    // hydrate flips the flag synchronously so the UI can disable
    // the picker until the cache is ready.
    expect(store.hydrating.value).toBe(true);
    await promise;
    expect(store.hydrating.value).toBe(false);

    expect(store.bookmarks.value["a@example.com"].notifyMode).toBe("never");
    expect(store.bookmarks.value["b@example.com"].notifyMode).toBe(null);
  });

  test("getMode reads from the cache; falls back to default when absent", () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "muted@example.com", name: null, autojoin: false, notifyMode: "never" },
    ]);

    expect(store.getMode("muted@example.com", "private-group")).toBe("never");
    expect(store.getMode("unknown@example.com", "private-group")).toBe("always");
    expect(store.getMode("unknown@example.com", "public-group")).toBe("on-mention");
  });

  test("setMode publishes via the WASM bridge and updates the cache", async () => {
    const store = createNotifySettingsStore();
    const fake = new FakeXmppClient([]);
    const result = await store.setMode(asClient(fake), {
      roomJid: "general@example.com",
      mode: "on-mention",
      name: "general",
    });
    expect(result).toBe("ok");
    expect(fake.published).toEqual([
      { roomJid: "general@example.com", mode: "on-mention", name: "general" },
    ]);
    expect(store.bookmarks.value["general@example.com"].notifyMode).toBe("on-mention");
  });

  test("setMode preserves cached entries for other rooms", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "other@example.com", name: "Other", autojoin: false, notifyMode: "always" },
    ]);
    const fake = new FakeXmppClient([]);
    await store.setMode(asClient(fake), { roomJid: "new@example.com", mode: "never" });
    expect(store.bookmarks.value["other@example.com"].notifyMode).toBe("always");
    expect(store.bookmarks.value["new@example.com"].notifyMode).toBe("never");
  });

  test("setMode failure leaves the rest of the cache unchanged (round-8 P2)", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "kept@example.com", name: "Kept", autojoin: false, notifyMode: "never" },
    ]);
    const rejecting = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async setRoomNotificationMode(): Promise<SetRoomNotificationModeOutcome> {
        return { kind: "error" };
      },
    };
    const result = await store.setMode(rejecting as unknown as BrowserXmppClient, {
      roomJid: "fails@example.com",
      mode: "always",
    });
    expect(result).toBe("error");
    // Failed publish must not poison the rest of the cache.
    expect(store.bookmarks.value["kept@example.com"].notifyMode).toBe("never");
    expect(store.bookmarks.value["fails@example.com"]).toBeUndefined();
  });

  test("setMode surfaces node-config-mismatch separately from generic errors", async () => {
    const store = createNotifySettingsStore();
    const mismatch = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async setRoomNotificationMode(): Promise<SetRoomNotificationModeOutcome> {
        return { kind: "node-config-mismatch" };
      },
    };
    const result = await store.setMode(mismatch as unknown as BrowserXmppClient, {
      roomJid: "old-node@example.com",
      mode: "never",
    });
    expect(result).toBe("node-config-mismatch");
    expect(store.bookmarks.value["old-node@example.com"]).toBeUndefined();
  });

  test("reset clears the cache for logout / account-switch (round-8 P1)", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "a@example.com", name: "A", autojoin: false, notifyMode: "never" },
      { jid: "b@example.com", name: "B", autojoin: false, notifyMode: "on-mention" },
    ]);
    expect(Object.keys(store.bookmarks.value).length).toBe(2);
    store.reset();
    expect(Object.keys(store.bookmarks.value).length).toBe(0);
    expect(store.hydrating.value).toBe(false);
  });

  test("hydrate is re-entrancy-guarded (no double-fetch race)", async () => {
    const store = createNotifySettingsStore();
    let calls = 0;
    let resolveFetch: (items: UserBookmarkItem[]) => void = () => {};
    const slowClient = {
      fetchUserBookmarks: () => {
        calls += 1;
        return new Promise<UserBookmarkItem[]>((resolve) => {
          resolveFetch = resolve;
        });
      },
      setRoomNotificationMode: async (): Promise<SetRoomNotificationModeOutcome> => ({
        kind: "error",
      }),
    };

    const first = store.hydrate(slowClient as unknown as BrowserXmppClient);
    // Second concurrent call: skipped silently while the first is
    // still in flight.
    const second = store.hydrate(slowClient as unknown as BrowserXmppClient);
    expect(calls).toBe(1);

    resolveFetch([{ jid: "x@example.com", name: "X", autojoin: false, notifyMode: "never" }]);
    await Promise.all([first, second]);
    expect(calls).toBe(1);
  });
});
