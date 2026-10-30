import { normalizeHomeServer } from "@/auth/home-server";
import { readSessionSnapshot, replaceSession } from "@/auth/session-store";

export { readSessionSnapshot, replaceSession, subscribeSessionChanges } from "@/auth/session-store";

const LEGACY_SESSION_STORAGE_KEY = "waddle.chat.session";
const PENDING_STORAGE_KEY = "waddle.chat.auth.pending.v1";
const RETURN_NONCE = "waddle_auth_state";
let consuming: Promise<void> | null = null;
let consumingWindow: Window | null = null;

export async function beginLoginRedirect(serverUrl: string, next: URL): Promise<boolean> {
  if (typeof window === "undefined" || normalizeHomeServer(serverUrl) !== serverUrl) return false;
  try {
    const snapshot = await readSessionSnapshot();
    const nonce = crypto.randomUUID();
    const pending = JSON.stringify({ serverUrl, nonce, revision: snapshot.revision });
    window.sessionStorage.setItem(PENDING_STORAGE_KEY, pending);
    if (window.sessionStorage.getItem(PENDING_STORAGE_KEY) !== pending) return false;
    next.searchParams.set(RETURN_NONCE, nonce);
    return true;
  } catch {
    return false;
  }
}

export function consumeRedirectSession(): Promise<void> {
  if (typeof window === "undefined") return Promise.resolve();
  if (consumingWindow !== window) { consumingWindow = window; consuming = null; }
  const url = new URL(window.location.href);
  const hashParams = new URLSearchParams(url.hash.replace(/^#/, ""));
  const sessionId = hashParams.get("waddle_session_id");
  const nonce = url.searchParams.get(RETURN_NONCE);
  url.searchParams.delete(RETURN_NONCE);
  url.searchParams.delete("waddle_server");
  hashParams.delete("waddle_session_id");
  const search = url.searchParams.toString();
  const hash = hashParams.toString();
  const cleaned = `${url.pathname}${search ? `?${search}` : ""}${hash ? `#${hash}` : ""}`;
  if (cleaned !== `${window.location.pathname}${window.location.search}${window.location.hash}`) {
    window.history.replaceState(null, "", cleaned);
  }
  try { window.localStorage.removeItem(LEGACY_SESSION_STORAGE_KEY); } catch { /* Never use the legacy token. */ }
  if (consuming) return consuming;
  if (!nonce) return Promise.resolve();
  let pending: Record<string, unknown>;
  try {
    const raw = window.sessionStorage.getItem(PENDING_STORAGE_KEY);
    if (!raw) return Promise.resolve();
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") return Promise.resolve();
    pending = value as Record<string, unknown>;
    if (pending.nonce !== nonce) return Promise.resolve();
    window.sessionStorage.removeItem(PENDING_STORAGE_KEY);
  } catch {
    return Promise.resolve();
  }
  if (typeof pending.serverUrl !== "string" || normalizeHomeServer(pending.serverUrl) !== pending.serverUrl
    || (pending.revision !== null && typeof pending.revision !== "string")
    || !sessionId) {
    consuming = Promise.reject(new Error("Invalid sign-in return"));
    return consuming;
  }

  const snapshot = { revision: pending.revision as string | null, session: null };
  const task = replaceSession(snapshot, { serverUrl: pending.serverUrl, sessionId }).then((replaced) => {
    if (!replaced) throw new Error("Session changed while sign-in was in progress");
  });
  const currentWindow = window;
  consuming = task.then(() => {
    if (consumingWindow === currentWindow) consuming = null;
  });
  return consuming;
}
