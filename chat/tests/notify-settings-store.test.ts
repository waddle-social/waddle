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
  resolveDefaultNotifyMode,
} from "../src/lib/notify-settings";
import type {
  BrowserXmppClient,
  NotifyMode,
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
  }): Promise<UserBookmarkItem | null> {
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
    return updated;
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
    const ok = await store.setMode(asClient(fake), {
      roomJid: "general@example.com",
      mode: "on-mention",
      name: "general",
    });
    expect(ok).toBe(true);
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

  test("setMode returns false when the WASM bridge rejects (resolves null)", async () => {
    const store = createNotifySettingsStore();
    const rejecting = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async setRoomNotificationMode(): Promise<UserBookmarkItem | null> {
        return null;
      },
    };
    const ok = await store.setMode(rejecting as unknown as BrowserXmppClient, {
      roomJid: "fails@example.com",
      mode: "always",
    });
    expect(ok).toBe(false);
    expect(store.bookmarks.value["fails@example.com"]).toBeUndefined();
  });
});
