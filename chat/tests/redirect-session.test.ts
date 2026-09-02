import { afterEach, describe, expect, test } from "bun:test";
import { createStorageMock } from "./helpers/mock-browser-storage";
import {
  clearStoredSessionId,
  consumeRedirectSession,
  getStoredSessionId,
} from "../src/auth/redirect-session";

const originalWindow = globalThis.window;
const originalLocalStorage = globalThis.localStorage;

afterEach(() => {
  if (originalLocalStorage === undefined) {
    Reflect.deleteProperty(globalThis, "localStorage");
  } else {
    (globalThis as typeof globalThis & { localStorage: Storage }).localStorage = originalLocalStorage;
  }
  if (originalWindow === undefined) {
    Reflect.deleteProperty(globalThis, "window");
  } else {
    (globalThis as typeof globalThis & { window: Window & typeof globalThis }).window = originalWindow;
  }
});

describe("consumeRedirectSession", () => {
  test("stores the redirect token and removes it from the visible URL", () => {
    const storage = createStorageMock();
    let currentUrl = new URL("https://chat.example/?waddle_server=https%3A%2F%2Fxmpp.example#view=dm&waddle_session_id=tok");
    const location = {
      get href() { return currentUrl.href; },
      get pathname() { return currentUrl.pathname; },
      get search() { return currentUrl.search; },
      get hash() { return currentUrl.hash; },
    } as Location;
    const history = {
      replaceState(_state: unknown, _title: string, url: string) {
        currentUrl = new URL(url, currentUrl.origin);
      },
    } as History;
    (globalThis as typeof globalThis & { localStorage: typeof storage }).localStorage = storage;
    (globalThis as typeof globalThis & { window: Window & { localStorage: typeof storage } }).window = {
      ...(originalWindow ?? {}),
      history,
      localStorage: storage,
      location,
    } as Window & { localStorage: typeof storage };

    expect(consumeRedirectSession()).toBe("tok");
    expect(getStoredSessionId()).toBe("tok");
    expect(window.location.href).toBe("https://chat.example/#view=dm");

    clearStoredSessionId();
    expect(getStoredSessionId()).toBeNull();
  });
});
