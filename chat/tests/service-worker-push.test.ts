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
  test("minimal XEP-0357 payload renders default title + no body preview, updates badge", async () => {
    // Canonical XEP-0357 publish carries only `message-count` plus
    // the `urn:waddle:push:context:0` routing context. No sender,
    // no body — preview is opt-in and out of the default path
    // (#528 acceptance criterion).
    const client = { postMessage: mock((_message: unknown) => {}) };
    const worker = loadServiceWorker([client]);
    const event = makePushEvent({
      json: () => ({
        "message-count": 6,
        context: {
          conversation: "space_channel@conference.example.com",
          thread: "",
          class: "personal_mention",
        },
      }),
    });

    worker.dispatch("push", event);
    await event.done();

    expect(worker.setAppBadge).toHaveBeenCalledWith(6);
    expect(client.postMessage).toHaveBeenCalledWith({
      type: "waddle:unread-count",
      unreadCount: 6,
    });
    // Default title is count-derived; body is the empty string (no
    // preview unless the server explicitly populated it).
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
        "message-count": 1,
        context: { conversation: "alice@example.com" },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    expect(worker.showNotification.mock.calls[0]?.[0]).toBe("1 new message");
  });

  test("opt-in preview keeps server-provided title/body", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        title: "Mention from @alice",
        body: "hello there",
        "message-count": 1,
        context: { conversation: "space_channel@conference.example.com" },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [title, options] = worker.showNotification.mock.calls[0] ?? [];
    expect(title).toBe("Mention from @alice");
    expect((options as NotificationOptions).body).toBe("hello there");
  });

  test("dm conversation routes to /dm/{username}", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        context: { conversation: "alice@example.com", class: "dm" },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe("/dm/alice");
  });

  test("dm with underscore in localpart routes as DM, not MUC", async () => {
    // Regression for an MUC heuristic that misrouted `jane_doe@example.com`
    // to `/jane/doe` (the underscore-as-MUC-separator fallback).
    // JIDs legally contain underscores; route on the typed `class`
    // field or the JID's domain prefix instead. Adversarial review
    // round-1 finding on PR #760.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        context: {
          conversation: "jane_doe@example.com",
          // `"dm"` matches DM_CLASS_VALUES exactly so the test is
          // independent of the domain heuristic. With `"direct"` (a
          // value the server never emits) the test would pass only
          // because `example.com` doesn't start with `muc.` — a
          // silent regression risk.
          class: "dm",
        },
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
        "message-count": 1,
        context: {
          conversation: "general@muc.example.com",
          class: "personal_mention",
        },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/r/general",
    );
  });

  test("thread context routes via ?thread= query string, not path segment", async () => {
    // Chat router carries threads in a search-param codec
    // (chat/src/router/codecs.ts::threadSearch), NOT a path segment.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        context: {
          conversation: "general@muc.example.com",
          thread: "thread-42",
          class: "personal_mention",
        },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/r/general?thread=thread-42",
    );
  });

  test("class='dm' overrides @muc. domain prefix", async () => {
    // Defense-in-depth: a misconfigured server that issues DMs from
    // a `muc.` subdomain MUST still route as DM when the typed
    // `class` field says so.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        context: {
          conversation: "alice@muc.example.com",
          class: "dm",
        },
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
        "message-count": 1,
        context: {
          conversation: "general@muc.example.com",
          class: "notify_all",
        },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/r/general",
    );
  });

  test("legacy data.url is honored when typed context is absent", async () => {
    // Mixed-version publishers (server pre-#528 emitter that doesn't
    // carry `context: { conversation }`) ship a deep link in
    // `data.url`. The SW must preserve that legacy behavior rather
    // than collapsing to `/`. ChatGPT Codex bot flagged this
    // regression on round-5.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        url: "/r/legacy-channel?thread=abc",
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/r/legacy-channel?thread=abc",
    );
  });

  test("legacy data.url is rejected if cross-origin", async () => {
    // Defense in depth: even on the legacy path, an off-origin
    // `data.url` must NOT be accepted. `sameOriginUrl` runs first.
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        url: "https://evil.example/steal",
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe("/");
  });

  test("clears the app badge when push message-count is zero", async () => {
    const client = { postMessage: mock((_message: unknown) => {}) };
    const worker = loadServiceWorker([client]);
    const event = makePushEvent({
      json: () => ({
        "message-count": 0,
        context: { conversation: "alice@example.com", class: "dm" },
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
