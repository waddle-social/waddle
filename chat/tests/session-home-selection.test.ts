import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { IDBFactory } from "fake-indexeddb";
import { createStorageMock } from "./helpers/mock-browser-storage";
import { useSessionAuth } from "../src/auth/session";
import { consumeRedirectSession, readSessionSnapshot, replaceSession } from "../src/auth/redirect-session";

const originalWindow = globalThis.window;
const originalIndexedDB = globalThis.indexedDB;
const originalFetch = globalThis.fetch;
let currentUrl: URL;

beforeEach(() => {
  globalThis.indexedDB = new IDBFactory();
  currentUrl = new URL("https://chat.example/");
  const localStorage = createStorageMock();
  const sessionStorage = createStorageMock();
  globalThis.window = {
    location: {
      get href() { return currentUrl.href; },
      set href(value: string) { currentUrl = new URL(value); },
      get origin() { return currentUrl.origin; },
      get pathname() { return currentUrl.pathname; },
      get search() { return currentUrl.search; },
      get hash() { return currentUrl.hash; },
    },
    history: { replaceState(_state: unknown, _title: string, url: string) { currentUrl = new URL(url, currentUrl.origin); } },
    localStorage, sessionStorage,
    addEventListener() {}, removeEventListener() {},
  } as unknown as Window & typeof globalThis;
});
afterEach(() => {
  globalThis.fetch = originalFetch;
  if (originalWindow === undefined) Reflect.deleteProperty(globalThis, "window");
  else globalThis.window = originalWindow;
  if (originalIndexedDB === undefined) Reflect.deleteProperty(globalThis, "indexedDB");
  else globalThis.indexedDB = originalIndexedDB;
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function session(id: string) {
  return { session_id: id, username: "david", avatar_url: null, xmpp_localpart: "david", jid: "david@example", xmpp_websocket_url: "wss://example/xmpp", is_expired: false, expires_at: null };
}

describe("home selection", () => {
  test("only publishes providers from the latest selected home", async () => {
    const a = deferred<Response>();
    const b = deferred<Response>();
    globalThis.fetch = ((input: string | URL | Request) => String(input).startsWith("https://a.example") ? a.promise : b.promise) as typeof fetch;
    const auth = useSessionAuth("https://default.example");
    const first = auth.fetchProviders("https://a.example");
    const second = auth.fetchProviders("https://b.example");
    b.resolve(Response.json([{ id: "b" }]));
    await second;
    a.resolve(Response.json([{ id: "a" }]));
    await first;
    expect(auth.activeServerUrl.value).toBe("https://b.example");
    expect(auth.defaultServerUrl).toBe("https://default.example");
    expect(auth.providers.value.map((provider) => provider.id)).toEqual(["b"]);
    auth.invalidateProviders();
    expect(auth.providers.value).toEqual([]);
  });

  test("login targets the selected home and binds its redirect to a nonce", async () => {
    globalThis.fetch = (async () => Response.json([{ id: "oidc" }])) as typeof fetch;
    const auth = useSessionAuth("https://default.example");
    await auth.fetchProviders("https://home.example/");
    await auth.login("https://other.example", "oidc");
    expect(currentUrl.origin).toBe("https://chat.example");
    await auth.login("https://home.example", "oidc");
    expect(currentUrl.origin).toBe("https://home.example");
    expect(currentUrl.pathname).toBe("/api/auth/start");
    expect(currentUrl.searchParams.get("session_transport")).toBe("fragment");
    const next = new URL(currentUrl.searchParams.get("next")!);
    expect(next.origin).toBe("https://chat.example");
    expect(next.searchParams.get("waddle_auth_state")).toBeTruthy();
    expect(next.searchParams.has("waddle_server")).toBe(false);
  });

  test("late logout cannot delete or send a newer home's credential to the old home", async () => {
    expect(await replaceSession(await readSessionSnapshot(), { serverUrl: "https://a.example", sessionId: "a-token" })).toBe(true);
    const requests: Array<{ url: string; init?: RequestInit }> = [];
    const logout = deferred<Response>();
    globalThis.fetch = ((input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      requests.push({ url, init });
      if (url === "https://a.example/api/auth/logout") return logout.promise;
      if (url.startsWith("https://a.example/api/auth/session")) return Promise.resolve(Response.json(session("a-token")));
      if (url.startsWith("https://b.example/api/auth/session")) return Promise.resolve(Response.json(session("b-token")));
      return Promise.resolve(Response.json([{ id: "oidc" }]));
    }) as typeof fetch;
    const auth = useSessionAuth("https://a.example");
    await auth.bootstrap();
    expect(auth.appState.value).toBe("ready");
    const loggingOut = auth.logout();
    await Promise.resolve();
    const snapshot = await readSessionSnapshot();
    expect(await replaceSession(snapshot, { serverUrl: "https://b.example", sessionId: "b-token" })).toBe(true);
    logout.resolve(new Response(null, { status: 204 }));
    await loggingOut;
    expect((await readSessionSnapshot()).session).toEqual({ serverUrl: "https://b.example", sessionId: "b-token" });
    expect(requests.some(({ url }) => url.startsWith("https://a.example") && url.includes("b-token"))).toBe(false);
    for (const request of requests.filter(({ url }) => url.includes("/api/auth/session") || url.includes("/api/auth/logout"))) {
      expect(request.init?.redirect).toBe("error");
    }
  });

  test("an explicit logout cannot be revived by a still-valid cookie", async () => {
    expect(await replaceSession(await readSessionSnapshot(), { serverUrl: "https://chat.example", sessionId: "a-token" })).toBe(true);
    let sessionRequests = 0;
    globalThis.fetch = ((input: string | URL | Request) => {
      const url = String(input);
      if (url.includes("/api/auth/logout")) return Promise.reject(new Error("network failure"));
      if (url.includes("/api/auth/session")) {
        sessionRequests += 1;
        return Promise.resolve(Response.json(session("a-token")));
      }
      return Promise.resolve(Response.json([{ id: "oidc" }]));
    }) as typeof fetch;
    const firstTab = useSessionAuth("https://chat.example");
    await firstTab.bootstrap();
    expect(firstTab.appState.value).toBe("ready");
    await firstTab.logout();
    expect((await readSessionSnapshot()).session).toBeNull();
    const reloadedTab = useSessionAuth("https://chat.example");
    await reloadedTab.bootstrap();
    expect(reloadedTab.appState.value).toBe("signed-out");
    expect(sessionRequests).toBe(1);
  });

  test("same-origin login survives a delayed logout clearing the shared cookie", async () => {
    expect(await replaceSession(await readSessionSnapshot(), { serverUrl: "https://chat.example", sessionId: "old" })).toBe(true);
    const logoutStarted = deferred<void>();
    const logoutResponse = deferred<Response>();
    const requests: string[] = [];
    let cookie: string | null = "old";
    globalThis.fetch = ((input: string | URL | Request) => {
      const url = new URL(String(input));
      requests.push(url.toString());
      if (url.pathname === "/api/auth/logout") {
        logoutStarted.resolve();
        return logoutResponse.promise;
      }
      if (url.pathname === "/api/auth/session") {
        const id = url.searchParams.get("session_id") ?? cookie;
        return Promise.resolve(id ? Response.json(session(id)) : new Response(null, { status: 401 }));
      }
      return Promise.resolve(Response.json([{ id: "oidc" }]));
    }) as typeof fetch;
    const oldTab = useSessionAuth("https://chat.example");
    await oldTab.bootstrap();
    const pendingLogout = oldTab.logout();
    await logoutStarted.promise;

    const newTab = useSessionAuth("https://chat.example");
    await newTab.fetchProviders("https://chat.example");
    await newTab.login("https://chat.example", "oidc");
    expect(currentUrl.searchParams.get("session_transport")).toBe("fragment");
    currentUrl = new URL(currentUrl.searchParams.get("next")!);
    currentUrl.hash = "waddle_session_id=new";
    cookie = "new";
    await consumeRedirectSession();
    cookie = null; // The older logout response clears the shared cookie.
    logoutResponse.resolve(new Response(null, { status: 204 }));
    await pendingLogout;
    await newTab.bootstrap();
    expect(newTab.appState.value).toBe("ready");
    expect(newTab.session.value?.session_id).toBe("new");
    expect(requests.some((url) => url.includes("/api/auth/session?session_id=new"))).toBe(true);
  });
});
