const SESSION_STORAGE_KEY = "waddle.chat.session";

export function getStoredSessionId() {
  if (typeof window === "undefined") return null;

  try {
    return window.localStorage.getItem(SESSION_STORAGE_KEY);
  } catch {
    return null;
  }
}

export function storeSessionId(sessionId: string) {
  if (typeof window === "undefined") return;

  try {
    window.localStorage.setItem(SESSION_STORAGE_KEY, sessionId);
  } catch {
    // ignore storage failures
  }
}

export function clearStoredSessionId() {
  if (typeof window === "undefined") return;

  try {
    window.localStorage.removeItem(SESSION_STORAGE_KEY);
  } catch {
    // ignore storage failures
  }
}

export function consumeRedirectSession() {
  if (typeof window === "undefined") return getStoredSessionId();

  const url = new URL(window.location.href);
  const hashParams = new URLSearchParams(url.hash.replace(/^#/, ""));
  const sessionId = hashParams.get("waddle_session_id");

  url.searchParams.delete("waddle_server");

  if (sessionId) {
    storeSessionId(sessionId);
    hashParams.delete("waddle_session_id");
  }

  const nextSearch = url.searchParams.toString();
  const nextHash = hashParams.toString();
  const cleanedUrl = `${url.pathname}${nextSearch ? `?${nextSearch}` : ""}${nextHash ? `#${nextHash}` : ""}`;
  const currentUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  if (cleanedUrl !== currentUrl) {
    window.history.replaceState(null, "", cleanedUrl);
  }

  return sessionId ?? getStoredSessionId();
}
