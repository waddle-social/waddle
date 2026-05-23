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
          class: "groupchat_personal_mention",
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
        url: "/space/channel",
        context: {
          conversation: "space_channel@conference.example.com",
          thread: undefined,
          class: "groupchat_personal_mention",
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

  test("dm conversation routes to /{username}", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        context: { conversation: "alice@example.com" },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe("/alice");
  });

  test("muc conversation routes to /{space}/{channel}", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        context: { conversation: "myspace_general@muc.example.com" },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/myspace/general",
    );
  });

  test("thread context routes to /{...}/threads/{thread}", async () => {
    const worker = loadServiceWorker();
    const event = makePushEvent({
      json: () => ({
        "message-count": 1,
        context: {
          conversation: "myspace_general@muc.example.com",
          thread: "thread-42",
          class: "groupchat_personal_mention",
        },
      }),
    });
    worker.dispatch("push", event);
    await event.done();

    const [, options] = worker.showNotification.mock.calls[0] ?? [];
    expect((options as NotificationOptions & { data: { url: string } }).data.url).toBe(
      "/myspace/general/threads/thread-42",
    );
  });

  test("clears the app badge when push message-count is zero", async () => {
    const client = { postMessage: mock((_message: unknown) => {}) };
    const worker = loadServiceWorker([client]);
    const event = makePushEvent({
      json: () => ({
        "message-count": 0,
        context: { conversation: "alice@example.com" },
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
});
