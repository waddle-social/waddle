import { afterEach, beforeEach, expect, test } from "bun:test";
import { IDBFactory } from "fake-indexeddb";
import { createStorageMock } from "./helpers/mock-browser-storage";
import { readSessionSnapshot, replaceSession, subscribeSessionChanges } from "../src/auth/session-store";

const originalWindow = globalThis.window;
const originalIndexedDB = globalThis.indexedDB;
const originalBroadcastChannel = globalThis.BroadcastChannel;
let storageHandler: ((event: StorageEvent) => void) | undefined;
let listeners: MockBroadcastChannel[];

class MockBroadcastChannel {
  onmessage: ((event: MessageEvent) => void) | null = null;
  constructor(public name: string) { listeners.push(this); }
  postMessage(data: unknown) {
    for (const target of listeners) {
      if (target !== this && target.name === this.name) target.onmessage?.({ data } as MessageEvent);
    }
  }
  close() { listeners = listeners.filter(item => item !== this); }
}

beforeEach(() => {
  listeners = [];
  storageHandler = undefined;
  globalThis.indexedDB = new IDBFactory();
  globalThis.BroadcastChannel = MockBroadcastChannel as unknown as typeof BroadcastChannel;
  globalThis.window = {
    localStorage: createStorageMock(),
    addEventListener(type: string, handler: (event: StorageEvent) => void) { if (type === "storage") storageHandler = handler; },
    removeEventListener() { storageHandler = undefined; },
  } as unknown as Window & typeof globalThis;
});

afterEach(() => {
  if (originalWindow === undefined) Reflect.deleteProperty(globalThis, "window"); else globalThis.window = originalWindow;
  if (originalIndexedDB === undefined) Reflect.deleteProperty(globalThis, "indexedDB"); else globalThis.indexedDB = originalIndexedDB;
  globalThis.BroadcastChannel = originalBroadcastChannel;
});

test("ignores own writes and deduplicates external revision hints across channels", async () => {
  let calls = 0;
  const unsubscribe = subscribeSessionChanges(() => { calls++; });
  expect(await replaceSession(await readSessionSnapshot(), { serverUrl: "https://home.example", sessionId: "token" })).toBe(true);
  expect(calls).toBe(0);
  const ownMessage = window.localStorage.getItem("waddle.chat.auth.changed");
  expect(ownMessage).toBeTruthy();
  storageHandler?.({ key: "waddle.chat.auth.changed", newValue: ownMessage } as StorageEvent);
  expect(calls).toBe(0);

  const sender = new MockBroadcastChannel("waddle.chat.auth.changed");
  const external = { sender: "another-document", revision: "revision-one" };
  sender.postMessage(external);
  storageHandler?.({ key: "waddle.chat.auth.changed", newValue: JSON.stringify(external) } as StorageEvent);
  sender.postMessage(external);
  expect(calls).toBe(1);
  const next = { sender: "another-document", revision: "revision-two" };
  storageHandler?.({ key: "waddle.chat.auth.changed", newValue: JSON.stringify(next) } as StorageEvent);
  sender.postMessage(next);
  expect(calls).toBe(2);
  storageHandler?.({ key: "waddle.chat.auth.changed", newValue: "invalid" } as StorageEvent);
  expect(calls).toBe(2);
  unsubscribe();
  sender.postMessage({ sender: "another-document", revision: "revision-three" });
  expect(calls).toBe(2);
  sender.close();
});
