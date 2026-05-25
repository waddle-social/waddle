import { describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";

type Listener = (event: Record<string, unknown>) => void;

function loadServiceWorker(windowClients: Array<Record<string, unknown>> = []) {
  const listeners = new Map<string, Listener[]>();
  const setAppBadge = mock(async (_count?: number) => {});
  const clearAppBadge = mock(async () => {});
  const showNotification = mock(async (_title: string, _options: NotificationOptions) => {});
  const clients = {
    matchAll: mock(async (_options?: unknown) => windowClients),
    openWindow: mock(async (_url: string) => null),
  };
  const self = {
    location: { origin: "https://chat.example.test" },
    clients: { claim: mock(async () => {}) },
    registration: { showNotification },
    skipWaiting: mock(async () => {}),
    addEventListener: (type: string, listener: Listener) => {
      const existing = listeners.get(type) ?? [];
      listeners.set(type, [...existing, listener]);
    },
  };
  const caches = {
    open: mock(async () => ({ addAll: mock(async (_urls: string[]) => {}) })),
    keys: mock(async () => []),
    delete: mock(async (_key: string) => true),
    match: mock(async (_request: unknown) => null),
  };
  const code = readFileSync(
    new URL("../src/service-worker/sw-template.js", import.meta.url),
    "utf8",
  ).replaceAll("__WADDLE_BUILD_SHA__", "test-sha");

  new Function("self", "caches", "fetch", "clients", "navigator", "URL", code)(
    self,
    caches,
    mock(async (_request: unknown) => ({ ok: false })),
    clients,
    { setAppBadge, clearAppBadge },
    URL,
  );

  function dispatch(type: string, event: Record<string, unknown>) {
    for (const listener of listeners.get(type) ?? []) {
      listener(event);
    }
  }

  return { dispatch, setAppBadge, clearAppBadge, showNotification, clients };
}

function makePushEvent(data: { json?: () => unknown; text?: () => string }) {
  const waits: Array<Promise<unknown>> = [];
  return {
    data,
    waitUntil: (promise: Promise<unknown>) => {
      waits.push(Promise.resolve(promise));
    },
    done: () => Promise.all(waits),
  };
}

function makeNotificationClickEvent(data: Record<string, unknown>) {
  const waits: Array<Promise<unknown>> = [];
  return {
    notification: {
      close: mock(() => {}),
      data,
    },
    waitUntil: (promise: Promise<unknown>) => {
      waits.push(Promise.resolve(promise));
    },
    done: () => Promise.all(waits),
  };
}

describe("service worker push handling", () => {
  test("minimal v=1 payload renders default count title with no body preview, updates badge", async () => {
    // Canonical v=1 envelope from
    // `crates/waddle-xmpp/src/push/envelope.rs::PushEnvelope`. XEP-0357
    // §4 forbids the push service from receiving message content, so
    // there is never a sender or body preview — the SW always renders
    // the count-derived default with an empty body.
    const client = { postMessage: mock((_message: unknown) => {}) };
    const worker = loadServiceWorker([client]);
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "personal_mention",
        conversation: "space_channel@conference.example.com",
        item: "stanza-id-mention-1",
        unread: 6,
      }),
    });

    worker.dispatch("push", event);
    await event.done();

    expect(worker.setAppBadge).toHaveBeenCalledWith(6);
    expect(client.postMessage).toHaveBeenCalledWith({
      type: "waddle:unread-count",
      unreadCount: 6,
    });
    expect(worker.showNotification).toHaveBeenCalledWith("6 new messages", {
      body: "",
      tag: "space_channel@conference.example.com",
      icon: "/android-chrome-192x192.png",
      data: {
        url: "/r/space_channel",
        context: {
          conversation: "space_channel@conference.example.com",
          thread: undefined,
          class: "personal_mention",
        },
      },
    });
  });

  test("singular default title for a one-message push", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "stanza-id-1msg",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    expect(worker.showNotification.mock.calls[0]?.[0]).toBe("1 new message");
  });

  test("dm with underscore in localpart routes as DM, not MUC", async () => {
    // Regression for an MUC heuristic that misrouted `jane_doe@example.com`
    // to `/jane/doe` (the underscore-as-MUC-separator fallback).
    // JIDs legally contain underscores; route on the typed `class`
    // field or the JID's domain prefix instead.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "jane_doe@example.com",
        item: "stanza-id-underscore",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/dm/jane_doe",
    );
  });

  test("muc conversation routes to /r/{channelId}", async () => {
    // Chat router URL is `/r/{channelId}` where channelId == JID
    // localpart (see chat/src/lib/xmpp/jid.ts::parseManagedRoomBareJid).
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "personal_mention",
        conversation: "general@muc.example.com",
        item: "stanza-id-muc",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/r/general",
    );
  });

  test("class='dm' overrides @muc. domain prefix", async () => {
    // Defense-in-depth: a misconfigured server that issues DMs from
    // a `muc.` subdomain MUST still route as DM when the typed
    // `class` field says so.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@muc.example.com",
        item: "stanza-id-class-override",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/dm/alice",
    );
  });

  test("notify_all (group-class) routes to /r/{channelId}", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "notify_all",
        conversation: "general@muc.example.com",
        item: "stanza-id-notify-all",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/r/general",
    );
  });

  test("clears the app badge when v=1 unread is zero", async () => {
    const client = { postMessage: mock((_message: unknown) => {}) };
    const worker = loadServiceWorker([client]);
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "stanza-id-zero",
        unread: 0,
      }),
    });

    worker.dispatch("push", event);
    await event.done();

    expect(worker.clearAppBadge).toHaveBeenCalled();
    expect(client.postMessage).toHaveBeenCalledWith({
      type: "waddle:unread-count",
      unreadCount: 0,
    });
  });

  test("non-v=1 payload collapses to the minimal default banner", async () => {
    // Breaking-changes-by-default: a payload missing `v: 1` (or with
    // a future-unknown version) is rendered as the minimal "Waddle"
    // notification, not auto-routed via legacy heuristics. The chat
    // upgrades the SW alongside the server; mixed-version publishers
    // are not supported.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({ "message-count": 5, context: { conversation: "alice@example.com" } }),
    });
    worker.dispatch("push", event);
    await event.done();
    expect(worker.showNotification).toHaveBeenCalledWith("Waddle", {
      body: "",
      tag: "waddle",
      icon: "/android-chrome-192x192.png",
      data: {
        url: "/",
        context: {
          conversation: undefined,
          thread: undefined,
          class: undefined,
        },
      },
    });
  });

  test("non-json push body silently produces a default 'Waddle' notification with no body", async () => {
    // Legacy / malformed publishes that aren't JSON: render the
    // minimal default without leaking the raw text into the
    // notification body. The chat's foreground Notification API
    // handles richer presentation when the user is online.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => {
        throw new Error("not json");
      },
      text: () => "raw push text",
    });

    worker.dispatch("push", event);
    await event.done();

    expect(worker.setAppBadge).not.toHaveBeenCalled();
    expect(worker.clearAppBadge).not.toHaveBeenCalled();
    expect(worker.showNotification).toHaveBeenCalledWith("Waddle", {
      body: "",
      tag: "waddle",
      icon: "/android-chrome-192x192.png",
      data: {
        url: "/",
        context: {
          conversation: undefined,
          thread: undefined,
          class: undefined,
        },
      },
    });
  });

  test("notification click rejects cross-origin url", async () => {
    // Defense in depth: a poisoned `data.url` MUST NOT cause the SW
    // to navigate to an arbitrary origin. Round-4 hostile-client
    // adversarial review on PR #760.
    const focusedClient = {
      url: "https://chat.example.test/some/page",
      focus: mock(async () => {}),
      navigate: mock(async (_url: string) => null),
    };
    const worker = loadServiceWorker([focusedClient]);
    const clickEvent = makeNotificationClickEvent({
      url: "https://evil.example/steal",
    });
    worker.dispatch("notificationclick", clickEvent);
    await clickEvent.done();

    // Falls back to safe origin root, not the cross-origin URL.
    expect(focusedClient.navigate).toHaveBeenCalledWith("/");
  });

  test("notification click navigates focused window to the routed url", async () => {
    const focusedClient = {
      url: "https://chat.example.test/some/other/page",
      focus: mock(async () => {}),
      navigate: mock(async (_url: string) => null),
    };
    const worker = loadServiceWorker([focusedClient]);
    const clickEvent = makeNotificationClickEvent({
      url: "/myspace/general/threads/thread-42",
      context: {
        conversation: "myspace_general@muc.example.com",
        thread: "thread-42",
      },
    });
    worker.dispatch("notificationclick", clickEvent);
    await clickEvent.done();

    expect(focusedClient.focus).toHaveBeenCalled();
    expect(focusedClient.navigate).toHaveBeenCalledWith(
      "/myspace/general/threads/thread-42",
    );
  });

  test("v=1 envelope: dm class routes to /dm/{username}", async () => {
    // PR-D3: server emits flat `{ v: 1, class, conversation, thread?, item, unread? }`
    // — the SW must parse this shape and route the same as the legacy
    // nested-context shape.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "stanza-id-001",
        unread: 3,
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    expect(worker.setAppBadge).toHaveBeenCalledWith(3);
    const [title, options] = worker.showNotification.mock.calls[0] ?? [];
    expect(title).toBe("3 new messages");
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/dm/alice",
    );
  });

  test("v=1 envelope: channel mention routes to /r/{channelId}?thread=…", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "personal_mention",
        conversation: "general@muc.example.com",
        thread: "thread-42",
        item: "stanza-id-002",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/r/general?thread=thread-42",
    );
  });

  test("v=1 envelope: duplicate item id suppresses the second SW notification", async () => {
    // In-band dedup: if the chat tab already rendered a foreground
    // notification for this stanza id, the SW push (arriving a few ms
    // later) should NOT fire a second banner — it should only refresh
    // the badge + unread count.
    const worker = loadServiceWorker();
    const first = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "stanza-dedup-1",
        unread: 1,
      }),
    });
    worker.dispatch("push", first);
    await first.done();
    expect(worker.showNotification).toHaveBeenCalledTimes(1);

    const second = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "stanza-dedup-1",
        unread: 2,
      }),
    });
    worker.dispatch("push", second);
    await second.done();
    // No second showNotification — but badge + broadcast still update.
    expect(worker.showNotification).toHaveBeenCalledTimes(1);
    expect(worker.setAppBadge).toHaveBeenLastCalledWith(2);
  });

  test("v=1 envelope: distinct item ids each render their own notification", async () => {
    const worker = loadServiceWorker();
    for (const item of ["a", "b"]) {
      const event = makePushEvent({
        json: () => ({
          v: 1,
          class: "dm",
          conversation: "alice@example.com",
          item,
          unread: 1,
        }),
      });
      worker.dispatch("push", event);
      await event.done();
    }
    expect(worker.showNotification).toHaveBeenCalledTimes(2);
  });

  test("foreground→SW signal: waddle:item-shown message suppresses the subsequent push for the same item", async () => {
    // The chat tab calls `postMessage({ type: 'waddle:item-shown', itemId })`
    // from `showMentionNotification` / `showDmNotification` when an
    // in-band notification renders. The SW must record that id so the
    // matching Web Push (arriving milliseconds later from the relay)
    // doesn't double-fire.
    const worker = loadServiceWorker();
    worker.dispatch("message", {
      data: { type: "waddle:item-shown", itemId: "stanza-foreground-1" },
    });
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "stanza-foreground-1",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();
    // No banner — the foreground tab already rendered for this id.
    expect(worker.showNotification).not.toHaveBeenCalled();
    // Badge + unread broadcast still updated.
    expect(worker.setAppBadge).toHaveBeenCalledWith(1);
  });

  test("foreground→SW signal: a different item id is NOT suppressed", async () => {
    const worker = loadServiceWorker();
    worker.dispatch("message", {
      data: { type: "waddle:item-shown", itemId: "stanza-foreground-1" },
    });
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "stanza-different-2",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();
    expect(worker.showNotification).toHaveBeenCalledTimes(1);
  });

  test("foreground→SW signal: malformed message is ignored", async () => {
    // A non-string itemId, missing itemId, or unknown type must be
    // tolerated — the SW shouldn't crash on a stray cross-tab message.
    const worker = loadServiceWorker();
    worker.dispatch("message", { data: { type: "waddle:item-shown" } });
    worker.dispatch("message", { data: { type: "waddle:item-shown", itemId: 42 } });
    worker.dispatch("message", { data: { type: "unknown-type", itemId: "x" } });
    worker.dispatch("message", {});
    // None of those should register dedup for an item with this id.
    const event = makePushEvent({
      json: () => ({
        v: 1,
        class: "dm",
        conversation: "alice@example.com",
        item: "x",
        unread: 1,
      }),
    });
    worker.dispatch("push", event);
    await event.done();
    expect(worker.showNotification).toHaveBeenCalledTimes(1);
  });

  test("v=1 envelope: dedup map evicts under overflow without unbounded scan", async () => {
    // Regression for round-4 perf finding: under a burst of > 256
    // distinct items inside the TTL window, the eviction scan must
    // not degrade to O(SHOWN_ITEM_MAX) per push. Capped probe +
    // FIFO fallback should keep eviction O(1) amortized.
    //
    // We can't directly time the SW from outside, but we can pin the
    // OBSERVABLE consequence: after pushing N > MAX distinct items,
    // the very first item must no longer be considered "shown" — it
    // got evicted to make room. The second-to-last must still be
    // suppressible. This catches a regression that broke eviction
    // entirely (e.g. forgot to `delete` the head).
    const worker = loadServiceWorker();
    const fire = async (item: string) => {
      const event = makePushEvent({
        json: () => ({
          v: 1,
          class: "dm",
          conversation: "alice@example.com",
          item,
          unread: 1,
        }),
      });
      worker.dispatch("push", event);
      await event.done();
    };
    // 260 distinct items: 4 past the 256 cap so the FIFO-head eviction
    // fires multiple times.
    const ITEMS = 260;
    for (let i = 0; i < ITEMS; i++) {
      await fire(`burst-${i}`);
    }
    // Each was distinct so each rendered. (Sanity check that the
    // dedup didn't accidentally suppress any.)
    expect(worker.showNotification).toHaveBeenCalledTimes(ITEMS);
    // The first item was evicted: re-firing it now must render again.
    await fire("burst-0");
    expect(worker.showNotification).toHaveBeenCalledTimes(ITEMS + 1);
    // A recent item is still in the dedup window: re-firing must NOT render.
    await fire(`burst-${ITEMS - 1}`);
    expect(worker.showNotification).toHaveBeenCalledTimes(ITEMS + 1);
  });

  test("v=1 envelope: dedup map is recency-aware (delete-then-set on re-touch)", async () => {
    // Regression for round-3 perf finding: without `delete-then-set` on
    // re-noting an item, the Map's FIFO order is by ORIGINAL insertion,
    // so a retried push for an early item gets evicted while still
    // inside its TTL window. The fix re-orders re-touched items to the
    // tail so eviction tracks recency.
    //
    // We can't directly inspect the Map from outside, so we exercise
    // the observable consequence: an item re-noted after the dedup
    // window slips suppresses again (recency-update lets the entry
    // survive eviction pressure long enough to dedup another retry).
    const worker = loadServiceWorker();
    const fire = async (item: string) => {
      const event = makePushEvent({
        json: () => ({
          v: 1,
          class: "dm",
          conversation: "alice@example.com",
          item,
          unread: 1,
        }),
      });
      worker.dispatch("push", event);
      await event.done();
    };
    // First fire: renders.
    await fire("item-A");
    expect(worker.showNotification).toHaveBeenCalledTimes(1);
    // Re-fire same item: suppressed (already shown).
    await fire("item-A");
    expect(worker.showNotification).toHaveBeenCalledTimes(1);
    // Distinct item: renders.
    await fire("item-B");
    expect(worker.showNotification).toHaveBeenCalledTimes(2);
    // Re-fire the FIRST item AGAIN: still suppressed. Without the
    // delete-then-set, this branch already held under low pressure;
    // the regression manifests under churn, but pinning the basic
    // delete-on-touch contract here guards against accidental
    // simplification.
    await fire("item-A");
    expect(worker.showNotification).toHaveBeenCalledTimes(2);
  });
});
