import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { IDBFactory } from "fake-indexeddb";
import { createStorageMock } from "./helpers/mock-browser-storage";
import { beginLoginRedirect, consumeRedirectSession, readSessionSnapshot, replaceSession } from "../src/auth/redirect-session";

const originalWindow = globalThis.window;
const originalIndexedDB = globalThis.indexedDB;
let currentUrl: URL;
let localStorage: Storage;
let sessionStorage: Storage;
beforeEach(() => {
  globalThis.indexedDB = new IDBFactory();
  currentUrl = new URL("https://chat.example/room?view=one#tab=chat");
  localStorage = createStorageMock();
  sessionStorage = createStorageMock();
  const location = {
    get href() { return currentUrl.href; },
    get origin() { return currentUrl.origin; },
    get pathname() { return currentUrl.pathname; },
    get search() { return currentUrl.search; },
    get hash() { return currentUrl.hash; },
  } as Location;
  globalThis.window = {
    location, localStorage, sessionStorage,
    history: { replaceState(_state: unknown, _title: string, url: string) { currentUrl = new URL(url, currentUrl.origin); } },
    addEventListener() {}, removeEventListener() {},
  } as unknown as Window & typeof globalThis;
});
afterEach(() => {
  if (originalWindow === undefined) Reflect.deleteProperty(globalThis, "window");
  else globalThis.window = originalWindow;
  if (originalIndexedDB === undefined) Reflect.deleteProperty(globalThis, "indexedDB");
  else globalThis.indexedDB = originalIndexedDB;
});

describe("bound session storage", () => {
  test("serializes competing updates and retains tombstones against ABA", async () => {
    const initial = await readSessionSnapshot();
    const [a, b] = await Promise.all([
      replaceSession(initial, { serverUrl: "https://a.example", sessionId: "a" }),
      replaceSession(initial, { serverUrl: "https://b.example", sessionId: "b" }),
    ]);
    expect([a, b].filter(Boolean)).toHaveLength(1);
    const active = await readSessionSnapshot();
    expect(active.session?.sessionId).toBe(a ? "a" : "b");
    expect(await replaceSession(active, null)).toBe(true);
    const tombstone = await readSessionSnapshot();
    expect(tombstone.session).toBeNull();
    expect(tombstone.revision).not.toBeNull();
    expect(await replaceSession(initial, { serverUrl: "https://a.example", sessionId: "stale" })).toBe(false);
    expect(await replaceSession(active, null)).toBe(false);
  });
  test("rejects malformed bindings", async () => {
    await expect(replaceSession(await readSessionSnapshot(), { serverUrl: "https://evil.example/path", sessionId: "secret" })).rejects.toThrow();
    await expect(replaceSession(await readSessionSnapshot(), { serverUrl: "https://home.example", sessionId: "" })).rejects.toThrow();
  });
});

describe("redirect session", () => {
  test("scrubs synchronously and binds a matching fragment to its pending home", async () => {
    localStorage.setItem("waddle.chat.session", "legacy");
    const next = new URL(currentUrl);
    expect(await beginLoginRedirect("https://home.example", next)).toBe(true);
    currentUrl = next;
    currentUrl.searchParams.set("waddle_server", "https://evil.example");
    currentUrl.hash = "tab=chat&waddle_session_id=token";
    const consuming = consumeRedirectSession();
    expect(currentUrl.href).toBe("https://chat.example/room?view=one#tab=chat");
    expect(localStorage.getItem("waddle.chat.session")).toBeNull();
    await Promise.all([consuming, consumeRedirectSession()]);
    expect((await readSessionSnapshot()).session).toEqual({ serverUrl: "https://home.example", sessionId: "token" });
    currentUrl = next;
    currentUrl.hash = "waddle_session_id=replay";
    await consumeRedirectSession();
    expect((await readSessionSnapshot()).session?.sessionId).toBe("token");
  });
  test("ignores unsolicited and mismatched fragments", async () => {
    currentUrl.hash = "waddle_session_id=unbound";
    await consumeRedirectSession();
    expect((await readSessionSnapshot()).session).toBeNull();
    const next = new URL(currentUrl);
    expect(await beginLoginRedirect("https://home.example", next)).toBe(true);
    next.searchParams.set("waddle_auth_state", "wrong");
    next.hash = "waddle_session_id=forged";
    currentUrl = next;
    await consumeRedirectSession();
    expect((await readSessionSnapshot()).session).toBeNull();
    expect(currentUrl.hash).toBe("");
  });
  test("requires a fragment even when a matching nonce is present", async () => {
    const next = new URL(currentUrl);
    expect(await beginLoginRedirect("https://home.example", next)).toBe(true);
    currentUrl = next;
    await expect(consumeRedirectSession()).rejects.toThrow("Invalid sign-in return");
    await expect(consumeRedirectSession()).rejects.toThrow("Invalid sign-in return");
    expect((await readSessionSnapshot()).session).toBeNull();
  });
  test("same-origin fragment survives a later cookie clear", async () => {
    const next = new URL(currentUrl);
    expect(await beginLoginRedirect("https://chat.example", next)).toBe(true);
    currentUrl = next;
    currentUrl.hash = "waddle_session_id=new-token";
    // No cookie is supplied to the consumer, as after a delayed logout response.
    await consumeRedirectSession();
    expect((await readSessionSnapshot()).session).toEqual({ serverUrl: "https://chat.example", sessionId: "new-token" });
  });
  test("does not overwrite a session changed during a redirect", async () => {
    const next = new URL(currentUrl);
    expect(await beginLoginRedirect("https://a.example", next)).toBe(true);
    const snapshot = await readSessionSnapshot();
    expect(await replaceSession(snapshot, { serverUrl: "https://b.example", sessionId: "b" })).toBe(true);
    currentUrl = next;
    currentUrl.hash = "waddle_session_id=a";
    await expect(consumeRedirectSession()).rejects.toThrow("Session changed");
    await expect(consumeRedirectSession()).rejects.toThrow("Session changed");
    expect((await readSessionSnapshot()).session).toEqual({ serverUrl: "https://b.example", sessionId: "b" });
  });
});
