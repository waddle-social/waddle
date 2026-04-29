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

describe("service worker push handling", () => {
  test("sets the app badge and posts unread count to open clients", async () => {
    const client = { postMessage: mock((_message: unknown) => {}) };
    const worker = loadServiceWorker([client]);
    const event = makePushEvent({
      json: () => ({
        title: "Mention",
        body: "hello",
        roomJid: "space_channel@conference.example.com",
        url: "/space/channel",
        "message-count": 6,
      }),
    });

    worker.dispatch("push", event);
    await event.done();

    expect(worker.setAppBadge).toHaveBeenCalledWith(6);
    expect(worker.clients.matchAll).toHaveBeenCalledWith({
      type: "window",
      includeUncontrolled: true,
    });
    expect(client.postMessage).toHaveBeenCalledWith({
      type: "waddle:unread-count",
      unreadCount: 6,
    });
    expect(worker.showNotification).toHaveBeenCalledWith("Mention", {
      body: "hello",
      tag: "space_channel@conference.example.com",
      icon: "/android-chrome-192x192.png",
      data: { url: "/space/channel" },
    });
  });

  test("clears the app badge when push unread count is zero", async () => {
    const client = { postMessage: mock((_message: unknown) => {}) };
    const worker = loadServiceWorker([client]);
    const event = makePushEvent({
      json: () => ({
        title: "Waddle",
        body: "",
        roomJid: "space_channel@conference.example.com",
        unreadCount: 0,
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

  test("keeps text notification fallback for non-json push payloads", async () => {
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
    expect(worker.clients.matchAll).not.toHaveBeenCalled();
    expect(worker.showNotification).toHaveBeenCalledWith("Waddle", {
      body: "raw push text",
      tag: undefined,
      icon: "/android-chrome-192x192.png",
      data: { url: "/" },
    });
  });
});
