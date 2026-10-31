import { normalizeHomeServer } from "@/auth/home-server";

export interface HomeSession {
  serverUrl: string;
  sessionId: string;
}

export interface SessionSnapshot {
  revision: string | null;
  session: HomeSession | null;
}

const DATABASE = "waddle.chat.auth";
const STORE = "sessions";
const KEY = "current";
const CHANNEL = "waddle.chat.auth.changed";
const NOTIFICATION_KEY = "waddle.chat.auth.changed";
const senderIds = new WeakMap<Window, string>();

function senderId(): string {
  let id = senderIds.get(window);
  if (!id) { id = crypto.randomUUID(); senderIds.set(window, id); }
  return id;
}

function validSession(value: unknown): value is HomeSession {
  if (!value || typeof value !== "object") return false;
  const item = value as Record<string, unknown>;
  return typeof item.serverUrl === "string"
    && normalizeHomeServer(item.serverUrl) === item.serverUrl
    && typeof item.sessionId === "string" && item.sessionId.length > 0;
}

function parseSnapshot(value: unknown): SessionSnapshot {
  if (value === undefined) return { revision: null, session: null };
  if (!value || typeof value !== "object") throw new Error("Invalid stored session");
  const item = value as Record<string, unknown>;
  if (typeof item.revision !== "string" || !item.revision || (item.session !== null && !validSession(item.session))) {
    throw new Error("Invalid stored session");
  }
  return { revision: item.revision, session: item.session as HomeSession | null };
}

function openDatabase(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onerror = () => reject(request.error ?? new Error("Failed to open session storage"));
    request.onblocked = () => reject(new Error("Session storage is blocked"));
    request.onsuccess = () => resolve(request.result);
  });
}

export async function readSessionSnapshot(): Promise<SessionSnapshot> {
  const db = await openDatabase();
  try {
    return await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, "readonly");
      const request = tx.objectStore(STORE).get(KEY);
      let snapshot: SessionSnapshot;
      request.onsuccess = () => {
        try { snapshot = parseSnapshot(request.result); } catch { tx.abort(); }
      };
      tx.oncomplete = () => resolve(snapshot);
      tx.onabort = tx.onerror = () => reject(tx.error ?? new Error("Failed to read session storage"));
    });
  } finally {
    db.close();
  }
}

function notifyChange(revision: string) {
  if (typeof window === "undefined") return;
  const message = { sender: senderId(), revision };
  try {
    const channel = new BroadcastChannel(CHANNEL);
    channel.postMessage(message);
    channel.close();
  } catch { /* Storage events are a fallback notification. */ }
  try { window.localStorage.setItem(NOTIFICATION_KEY, JSON.stringify(message)); } catch { /* Notifications are best effort. */ }
}

export async function replaceSession(snapshot: SessionSnapshot, session: HomeSession | null): Promise<boolean> {
  if (session !== null && !validSession(session)) throw new Error("Invalid session binding");
  const db = await openDatabase();
  try {
    let committedRevision: string | null = null;
    const replaced = await new Promise<boolean>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      const request = store.get(KEY);
      let didReplace = false;
      request.onsuccess = () => {
        try {
          const current = parseSnapshot(request.result);
          if (current.revision !== snapshot.revision) return;
          committedRevision = crypto.randomUUID();
          store.put({ revision: committedRevision, session }, KEY);
          didReplace = true;
        } catch { tx.abort(); }
      };
      tx.oncomplete = () => resolve(didReplace);
      tx.onabort = tx.onerror = () => reject(tx.error ?? new Error("Failed to update session storage"));
    });
    if (replaced && committedRevision) notifyChange(committedRevision);
    return replaced;
  } finally {
    db.close();
  }
}

export function subscribeSessionChanges(callback: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  const ownSender = senderId();
  const observed = new Set<string>();
  const receive = (value: unknown) => {
    if (!value || typeof value !== "object") return;
    const item = value as Record<string, unknown>;
    if (typeof item.sender !== "string" || !item.sender || item.sender === ownSender
      || typeof item.revision !== "string" || !item.revision || observed.has(item.revision)) return;
    observed.add(item.revision);
    if (observed.size > 64) observed.delete(observed.values().next().value!);
    callback();
  };
  let channel: BroadcastChannel | undefined;
  try {
    channel = new BroadcastChannel(CHANNEL);
    channel.onmessage = event => receive(event.data);
  } catch { /* Storage events provide a fallback. */ }
  const onStorage = (event: StorageEvent) => {
    if (event.key !== NOTIFICATION_KEY || !event.newValue) return;
    try { receive(JSON.parse(event.newValue)); } catch { /* Ignore invalid hints. */ }
  };
  window.addEventListener("storage", onStorage);
  return () => {
    channel?.close();
    window.removeEventListener("storage", onStorage);
  };
}
