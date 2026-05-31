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
  DmBookmarkItem,
  NotifyMode,
  SetDmNotificationModeResult,
  SetRoomNotificationModeOutcome,
  UserBookmarkItem,
} from "../src/lib/xmpp-client";

class FakeXmppClient {
  constructor(
    private bookmarks: UserBookmarkItem[],
    public published: {
      roomJid: string;
      mode: NotifyMode;
      name?: string;
      richPayloadOptIn?: boolean;
    }[] = [],
    private dmBookmarks: DmBookmarkItem[] = [],
    public dmPublished: {
      dmJid: string;
      mode: NotifyMode;
      richPayloadOptIn: boolean;
    }[] = [],
  ) {}

  async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
    return [...this.bookmarks];
  }

  async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
    return [...this.dmBookmarks];
  }

  async setRoomNotificationMode(opts: {
    roomJid: string;
    mode: NotifyMode;
    name?: string;
    richPayloadOptIn?: boolean;
  }): Promise<SetRoomNotificationModeOutcome> {
    this.published.push(opts);
    // Mirror the WASM bridge: the opt-in defaults to false when the
    // caller omits it (#719).
    const richPayloadOptIn = opts.richPayloadOptIn ?? false;
    const updated: UserBookmarkItem = {
      jid: opts.roomJid,
      name: opts.name ?? null,
      autojoin: false,
      notifyMode: opts.mode,
      richPayloadOptIn,
    };
    const idx = this.bookmarks.findIndex((b) => b.jid === opts.roomJid);
    if (idx >= 0) {
      this.bookmarks[idx] = {
        ...this.bookmarks[idx],
        notifyMode: opts.mode,
        richPayloadOptIn,
      };
    } else {
      this.bookmarks.push(updated);
    }
    return { kind: "ok", item: updated };
  }

  async setDmNotificationMode(opts: {
    dmJid: string;
    mode: NotifyMode;
    richPayloadOptIn: boolean;
  }): Promise<SetDmNotificationModeResult> {
    this.dmPublished.push(opts);
    const item: DmBookmarkItem = {
      jid: opts.dmJid,
      notifyMode: opts.mode,
      richPayloadOptIn: opts.richPayloadOptIn,
    };
    const idx = this.dmBookmarks.findIndex((b) => b.jid === opts.dmJid);
    if (idx >= 0) this.dmBookmarks[idx] = item;
    else this.dmBookmarks.push(item);
    return { kind: "ok", item };
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
      richPayloadOptIn: false,
    };
    expect(effectiveNotifyMode(bookmark, "public-group")).toBe("never");
  });

  test("falls back to XEP-0492 §3 default when bookmark has no mode", () => {
    const bookmark: UserBookmarkItem = {
      jid: "room@example.com",
      name: null,
      autojoin: false,
      notifyMode: null,
      richPayloadOptIn: false,
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
      { jid: "a@example.com", name: "A", autojoin: false, notifyMode: "never", richPayloadOptIn: false },
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
      fetchDmBookmarks: async () => [] as DmBookmarkItem[],
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
      { jid: "a@example.com", name: "A", autojoin: false, notifyMode: "always", richPayloadOptIn: false },
      { jid: "b@example.com", name: "B", autojoin: false, notifyMode: "never", richPayloadOptIn: false },
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
      { jid: "a@example.com", name: "A", autojoin: true, notifyMode: "never", richPayloadOptIn: false },
      { jid: "b@example.com", name: "B", autojoin: false, notifyMode: null, richPayloadOptIn: false },
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
      { jid: "muted@example.com", name: null, autojoin: false, notifyMode: "never", richPayloadOptIn: false },
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
      kind: "private-group",
      name: "general",
    });
    expect(result).toBe("ok");
    // No prior bookmark → the opt-in defaults to false (minimal payload).
    expect(fake.published).toEqual([
      { roomJid: "general@example.com", mode: "on-mention", name: "general", richPayloadOptIn: false },
    ]);
    expect(store.bookmarks.value["general@example.com"].notifyMode).toBe("on-mention");
  });

  test("setMode preserves cached entries for other rooms", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "other@example.com", name: "Other", autojoin: false, notifyMode: "always", richPayloadOptIn: false },
    ]);
    const fake = new FakeXmppClient([]);
    await store.setMode(asClient(fake), {
      roomJid: "new@example.com",
      mode: "never",
      kind: "private-group",
    });
    expect(store.bookmarks.value["other@example.com"].notifyMode).toBe("always");
    expect(store.bookmarks.value["new@example.com"].notifyMode).toBe("never");
  });

  test("setMode failure leaves the rest of the cache unchanged (round-8 P2)", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "kept@example.com", name: "Kept", autojoin: false, notifyMode: "never", richPayloadOptIn: false },
    ]);
    const rejecting = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
        return [];
      },
      async setRoomNotificationMode(): Promise<SetRoomNotificationModeOutcome> {
        return { kind: "error" };
      },
    };
    const result = await store.setMode(rejecting as unknown as BrowserXmppClient, {
      roomJid: "fails@example.com",
      mode: "always",
      kind: "private-group",
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
      async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
        return [];
      },
      async setRoomNotificationMode(): Promise<SetRoomNotificationModeOutcome> {
        return { kind: "node-config-mismatch" };
      },
    };
    const result = await store.setMode(mismatch as unknown as BrowserXmppClient, {
      roomJid: "old-node@example.com",
      mode: "never",
      kind: "private-group",
    });
    expect(result).toBe("node-config-mismatch");
    expect(store.bookmarks.value["old-node@example.com"]).toBeUndefined();
  });

  test("hydrate swallows a thrown rejection (round-14 PR review)", async () => {
    // Pin the defence-in-depth: a lower-layer regression that
    // throws (e.g. unwrapped requireConnectedXmpp() rejection)
    // MUST NOT propagate as an unhandled Promise rejection out of
    // the lifecycle handler's `void` dispatch.
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "kept@example.com", name: "Kept", autojoin: false, notifyMode: "always", richPayloadOptIn: false },
    ]);
    const throwing = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        throw new Error("session not ready");
      },
      async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
        return [];
      },
      async setRoomNotificationMode(): Promise<SetRoomNotificationModeOutcome> {
        return { kind: "error" };
      },
    };
    // Must resolve, NOT reject.
    await store.hydrate(throwing as unknown as BrowserXmppClient);
    expect(store.hydrating.value).toBe(false);
    // Cache untouched — the failed fetch never committed.
    expect(store.bookmarks.value["kept@example.com"].notifyMode).toBe("always");
  });

  test("hydrate-during-reset: stale fetch result is discarded (round-10 P1)", async () => {
    const store = createNotifySettingsStore();
    let resolveFetch: (items: UserBookmarkItem[]) => void = () => {};
    const slowClient = {
      fetchUserBookmarks: () =>
        new Promise<UserBookmarkItem[]>((resolve) => {
          resolveFetch = resolve;
        }),
      fetchDmBookmarks: async () => [] as DmBookmarkItem[],
      setRoomNotificationMode: async (): Promise<SetRoomNotificationModeOutcome> => ({
        kind: "error",
      }),
    };
    const pending = store.hydrate(slowClient as unknown as BrowserXmppClient);
    expect(store.hydrating.value).toBe(true);

    // User logs out mid-fetch.
    store.reset();
    expect(store.hydrating.value).toBe(false);

    // Old account's fetch now resolves.
    resolveFetch([
      { jid: "stale@example.com", name: "stale", autojoin: false, notifyMode: "always", richPayloadOptIn: false },
    ]);
    await pending;

    // The stale commit MUST be dropped — generation bumped on reset.
    expect(store.bookmarks.value["stale@example.com"]).toBeUndefined();
  });

  test("setMode maps a thrown rejection to typed error (round-13 PR review)", async () => {
    // Pin the defence-in-depth: a lower-layer regression that
    // throws (e.g. unwrapped requireConnectedXmpp() rejection)
    // MUST surface as a typed "error" result, not propagate as an
    // exception that the UI doesn't translate into the banner.
    const store = createNotifySettingsStore();
    const throwing = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
        return [];
      },
      async setRoomNotificationMode(): Promise<SetRoomNotificationModeOutcome> {
        throw new Error("session not ready");
      },
    };
    const result = await store.setMode(throwing as unknown as BrowserXmppClient, {
      roomJid: "throws@example.com",
      mode: "always",
      kind: "private-group",
    });
    expect(result).toBe("error");
    expect(store.bookmarks.value["throws@example.com"]).toBeUndefined();
  });

  test("setMode-during-reset: stale publish result is discarded (round-12 P1)", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "kept@example.com", name: "Kept", autojoin: false, notifyMode: "always", richPayloadOptIn: false },
    ]);
    let resolvePublish: (outcome: SetRoomNotificationModeOutcome) => void = () => {};
    const slowClient = {
      fetchUserBookmarks: async () => [] as UserBookmarkItem[],
      fetchDmBookmarks: async () => [] as DmBookmarkItem[],
      setRoomNotificationMode: (): Promise<SetRoomNotificationModeOutcome> =>
        new Promise((resolve) => {
          resolvePublish = resolve;
        }),
    };

    const pending = store.setMode(slowClient as unknown as BrowserXmppClient, {
      roomJid: "stale@example.com",
      mode: "never",
      kind: "private-group",
    });

    // User logs out / switches accounts mid-publish.
    store.reset();
    expect(store.bookmarks.value["kept@example.com"]).toBeUndefined();

    // Old account's publish now resolves successfully.
    resolvePublish({
      kind: "ok",
      item: { jid: "stale@example.com", name: null, autojoin: false, notifyMode: "never", richPayloadOptIn: false },
    });
    await pending;

    // Stale publish MUST NOT commit into the post-reset cache.
    expect(store.bookmarks.value["stale@example.com"]).toBeUndefined();
  });

  test("reset clears the cache for logout / account-switch (round-8 P1)", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "a@example.com", name: "A", autojoin: false, notifyMode: "never", richPayloadOptIn: false },
      { jid: "b@example.com", name: "B", autojoin: false, notifyMode: "on-mention", richPayloadOptIn: false },
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
      fetchDmBookmarks: async () => [] as DmBookmarkItem[],
      setRoomNotificationMode: async (): Promise<SetRoomNotificationModeOutcome> => ({
        kind: "error",
      }),
    };

    const first = store.hydrate(slowClient as unknown as BrowserXmppClient);
    // Second concurrent call: skipped silently while the first is
    // still in flight.
    const second = store.hydrate(slowClient as unknown as BrowserXmppClient);
    expect(calls).toBe(1);

    resolveFetch([
      { jid: "x@example.com", name: "X", autojoin: false, notifyMode: "never", richPayloadOptIn: false },
    ]);
    await Promise.all([first, second]);
    expect(calls).toBe(1);
  });
});

describe("rich-payload opt-in (#719)", () => {
  test("getRichPayloadOptIn defaults to false and reads the cache", () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "rich@example.com", name: "Rich", autojoin: false, notifyMode: "always", richPayloadOptIn: true },
      { jid: "plain@example.com", name: "Plain", autojoin: false, notifyMode: "always", richPayloadOptIn: false },
    ]);
    expect(store.getRichPayloadOptIn("rich@example.com")).toBe(true);
    expect(store.getRichPayloadOptIn("plain@example.com")).toBe(false);
    // Absent bookmark — opt-out default (minimal payload).
    expect(store.getRichPayloadOptIn("unknown@example.com")).toBe(false);
  });

  test("setRichPayloadOptIn publishes the opt-in while preserving the current mode", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "room@example.com", name: "Room", autojoin: false, notifyMode: "on-mention", richPayloadOptIn: false },
    ]);
    const fake = new FakeXmppClient([]);
    const result = await store.setRichPayloadOptIn(asClient(fake), {
      roomJid: "room@example.com",
      optIn: true,
      kind: "private-group",
    });
    expect(result).toBe("ok");
    // The publish carries the EXISTING mode (on-mention), not a reset
    // to the conversation-kind default — only the opt-in changed.
    expect(fake.published).toEqual([
      { roomJid: "room@example.com", mode: "on-mention", name: undefined, richPayloadOptIn: true },
    ]);
    expect(store.getRichPayloadOptIn("room@example.com")).toBe(true);
    expect(store.getMode("room@example.com", "private-group")).toBe("on-mention");
  });

  test("setRichPayloadOptIn on an unknown room uses the XEP-0492 §3 default mode", async () => {
    const store = createNotifySettingsStore();
    const fake = new FakeXmppClient([]);
    await store.setRichPayloadOptIn(asClient(fake), {
      roomJid: "fresh@example.com",
      optIn: true,
      kind: "public-group",
      name: "Fresh",
    });
    expect(fake.published).toEqual([
      { roomJid: "fresh@example.com", mode: "on-mention", name: "Fresh", richPayloadOptIn: true },
    ]);
    expect(store.getRichPayloadOptIn("fresh@example.com")).toBe(true);
  });

  test("setMode preserves the existing rich-payload opt-in", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "room@example.com", name: "Room", autojoin: false, notifyMode: "always", richPayloadOptIn: true },
    ]);
    const fake = new FakeXmppClient([]);
    await store.setMode(asClient(fake), {
      roomJid: "room@example.com",
      mode: "never",
      kind: "private-group",
    });
    // Changing the mode MUST NOT silently drop the opt-in.
    expect(fake.published).toEqual([
      { roomJid: "room@example.com", mode: "never", name: undefined, richPayloadOptIn: true },
    ]);
    expect(store.getRichPayloadOptIn("room@example.com")).toBe(true);
  });
});

describe("per-DM notification settings (#720)", () => {
  test("hydrate merges MUC bookmarks and per-DM entries into one cache", async () => {
    const store = createNotifySettingsStore();
    const fake = new FakeXmppClient(
      [{ jid: "room@example.com", name: "Room", autojoin: false, notifyMode: "on-mention", richPayloadOptIn: false }],
      [],
      [{ jid: "alice@example.com", notifyMode: "never", richPayloadOptIn: true }],
    );

    await store.hydrate(asClient(fake));

    // MUC bookmark survives the merge.
    expect(store.bookmarks.value["room@example.com"].notifyMode).toBe("on-mention");
    // DM entry is adapted into the cache's UserBookmarkItem shape:
    // no room name / autojoin, but the mode and opt-in carry over.
    const dm = store.bookmarks.value["alice@example.com"];
    expect(dm.notifyMode).toBe("never");
    expect(dm.richPayloadOptIn).toBe(true);
    expect(dm.name).toBe(null);
    expect(dm.autojoin).toBe(false);
    // The kind-driven resolver reads the DM entry like any other.
    expect(store.getMode("alice@example.com", "direct-chat")).toBe("never");
    expect(store.getRichPayloadOptIn("alice@example.com")).toBe(true);
  });

  test("getMode falls back to the direct-chat §3 default (always) when no DM entry", () => {
    const store = createNotifySettingsStore();
    expect(store.getMode("bob@example.com", "direct-chat")).toBe("always");
  });

  test("setMode on a direct-chat routes to setDmNotificationMode, not the MUC bridge", async () => {
    const store = createNotifySettingsStore();
    const fake = new FakeXmppClient([]);
    const result = await store.setMode(asClient(fake), {
      roomJid: "alice@example.com",
      mode: "on-mention",
      kind: "direct-chat",
    });
    expect(result).toBe("ok");
    // The publish went to the DM carrier (urn:waddle:dm-bookmarks:0),
    // NOT the XEP-0402 room bridge.
    expect(fake.published).toEqual([]);
    expect(fake.dmPublished).toEqual([
      { dmJid: "alice@example.com", mode: "on-mention", richPayloadOptIn: false },
    ]);
    expect(store.getMode("alice@example.com", "direct-chat")).toBe("on-mention");
  });

  test("setMode on a direct-chat preserves the cached rich-payload opt-in", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "alice@example.com", name: null, autojoin: false, notifyMode: "always", richPayloadOptIn: true },
    ]);
    const fake = new FakeXmppClient([]);
    await store.setMode(asClient(fake), {
      roomJid: "alice@example.com",
      mode: "never",
      kind: "direct-chat",
    });
    // Changing the DM mode must re-send the cached opt-in (#719).
    expect(fake.dmPublished).toEqual([
      { dmJid: "alice@example.com", mode: "never", richPayloadOptIn: true },
    ]);
    expect(store.getRichPayloadOptIn("alice@example.com")).toBe(true);
  });

  test("setRichPayloadOptIn on a direct-chat routes to the DM bridge", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "alice@example.com", name: null, autojoin: false, notifyMode: "on-mention", richPayloadOptIn: false },
    ]);
    const fake = new FakeXmppClient([]);
    const result = await store.setRichPayloadOptIn(asClient(fake), {
      roomJid: "alice@example.com",
      optIn: true,
      kind: "direct-chat",
    });
    expect(result).toBe("ok");
    // Flipping the opt-in republishes the DM's current mode unchanged.
    expect(fake.published).toEqual([]);
    expect(fake.dmPublished).toEqual([
      { dmJid: "alice@example.com", mode: "on-mention", richPayloadOptIn: true },
    ]);
    expect(store.getRichPayloadOptIn("alice@example.com")).toBe(true);
  });

  test("removed outcome clears the DM cache entry and still returns ok", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "alice@example.com", name: null, autojoin: false, notifyMode: "never", richPayloadOptIn: false },
      { jid: "room@example.com", name: "Room", autojoin: false, notifyMode: "on-mention", richPayloadOptIn: false },
    ]);
    // Reverting to the §3 default (always) retracts the PEP item.
    const removing = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
        return [];
      },
      async setDmNotificationMode(): Promise<SetDmNotificationModeResult> {
        return { kind: "removed", jid: "alice@example.com" };
      },
    };
    const result = await store.setMode(removing as unknown as BrowserXmppClient, {
      roomJid: "alice@example.com",
      mode: "always",
      kind: "direct-chat",
    });
    expect(result).toBe("ok");
    // The DM entry is dropped → the resolver falls back to the default.
    expect(store.bookmarks.value["alice@example.com"]).toBeUndefined();
    expect(store.getMode("alice@example.com", "direct-chat")).toBe("always");
    // Unrelated MUC entry untouched.
    expect(store.bookmarks.value["room@example.com"].notifyMode).toBe("on-mention");
  });

  test("setMode on a direct-chat maps node-config-mismatch through unchanged", async () => {
    const store = createNotifySettingsStore();
    const mismatch = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
        return [];
      },
      async setDmNotificationMode(): Promise<SetDmNotificationModeResult> {
        return { kind: "node-config-mismatch" };
      },
    };
    const result = await store.setMode(mismatch as unknown as BrowserXmppClient, {
      roomJid: "alice@example.com",
      mode: "never",
      kind: "direct-chat",
    });
    expect(result).toBe("node-config-mismatch");
    expect(store.bookmarks.value["alice@example.com"]).toBeUndefined();
  });

  test("setDmNotificationMode that throws maps to a typed error result", async () => {
    const store = createNotifySettingsStore();
    const throwing = {
      async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
        return [];
      },
      async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
        return [];
      },
      async setDmNotificationMode(): Promise<SetDmNotificationModeResult> {
        throw new Error("bad dmJid");
      },
    };
    const result = await store.setMode(throwing as unknown as BrowserXmppClient, {
      roomJid: "alice@example.com",
      mode: "never",
      kind: "direct-chat",
    });
    expect(result).toBe("error");
    expect(store.bookmarks.value["alice@example.com"]).toBeUndefined();
  });

  test("removed outcome during reset does not resurrect the cleared cache", async () => {
    const store = createNotifySettingsStore();
    store.replaceAll([
      { jid: "kept@example.com", name: "Kept", autojoin: false, notifyMode: "always", richPayloadOptIn: false },
    ]);
    let resolvePublish: (outcome: SetDmNotificationModeResult) => void = () => {};
    const slowClient = {
      fetchUserBookmarks: async () => [] as UserBookmarkItem[],
      fetchDmBookmarks: async () => [] as DmBookmarkItem[],
      setDmNotificationMode: (): Promise<SetDmNotificationModeResult> =>
        new Promise((resolve) => {
          resolvePublish = resolve;
        }),
    };

    const pending = store.setMode(slowClient as unknown as BrowserXmppClient, {
      roomJid: "alice@example.com",
      mode: "never",
      kind: "direct-chat",
    });

    // User logs out / switches accounts mid-publish.
    store.reset();
    expect(store.bookmarks.value["kept@example.com"]).toBeUndefined();

    // Old account's publish now resolves with an ok item.
    resolvePublish({
      kind: "ok",
      item: { jid: "alice@example.com", notifyMode: "never", richPayloadOptIn: false },
    });
    await pending;

    // Stale publish MUST NOT commit into the post-reset cache.
    expect(store.bookmarks.value["alice@example.com"]).toBeUndefined();
  });
});
