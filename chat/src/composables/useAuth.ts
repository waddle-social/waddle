import { ref, type Ref } from "vue";
import { createServerAuth, type WaddleSession, type AuthProvider } from "@/lib/server-auth";
import { WaddleApi } from "@/lib/waddle-api";
import type { AppState } from "@/lib/chat-ui";

const ACTIVE_SERVER_STORAGE_KEY = "waddle.chat.active-server";
const SESSION_STORAGE_KEY = "waddle.chat.sessions";

type SessionMap = Record<string, string>;

function trimTrailingSlash(value: string) {
  return value.endsWith("/") ? value.slice(0, -1) : value;
}

function normalizeServerUrl(value: string) {
  return trimTrailingSlash(value.trim());
}

function readSessionMap(): SessionMap {
  if (typeof window === "undefined") {
    return {};
  }

  try {
    const raw = window.localStorage.getItem(SESSION_STORAGE_KEY);
    if (!raw) {
      return {};
    }

    const parsed = JSON.parse(raw) as unknown;
    return parsed && typeof parsed === "object" ? (parsed as SessionMap) : {};
  } catch {
    return {};
  }
}

function writeSessionMap(sessions: SessionMap) {
  if (typeof window === "undefined") {
    return;
  }

  try {
    window.localStorage.setItem(SESSION_STORAGE_KEY, JSON.stringify(sessions));
  } catch {
    // ignore storage failures
  }
}

function getStoredSessionId(serverUrl: string) {
  return readSessionMap()[normalizeServerUrl(serverUrl)] ?? null;
}

function storeSessionId(serverUrl: string, sessionId: string) {
  const sessions = readSessionMap();
  sessions[normalizeServerUrl(serverUrl)] = sessionId;
  writeSessionMap(sessions);
}

function clearStoredSessionId(serverUrl: string) {
  const sessions = readSessionMap();
  delete sessions[normalizeServerUrl(serverUrl)];
  writeSessionMap(sessions);
}

function loadActiveServer(defaultServerUrl: string) {
  if (typeof window === "undefined") {
    return defaultServerUrl;
  }

  try {
    return normalizeServerUrl(
      window.localStorage.getItem(ACTIVE_SERVER_STORAGE_KEY) || defaultServerUrl,
    );
  } catch {
    return defaultServerUrl;
  }
}

function saveActiveServer(serverUrl: string) {
  if (typeof window === "undefined") {
    return;
  }

  try {
    window.localStorage.setItem(ACTIVE_SERVER_STORAGE_KEY, normalizeServerUrl(serverUrl));
  } catch {
    // ignore storage failures
  }
}

function buildReturnUrl(currentUrl: string, serverUrl: string, defaultServerUrl: string) {
  const url = new URL(currentUrl);
  const active = normalizeServerUrl(serverUrl);
  const defaultServer = normalizeServerUrl(defaultServerUrl);

  if (active !== defaultServer) {
    url.searchParams.set("waddle_server", active);
  } else {
    url.searchParams.delete("waddle_server");
  }

  const hashParams = new URLSearchParams(url.hash.replace(/^#/, ""));
  hashParams.delete("waddle_session_id");
  const hash = hashParams.toString();
  url.hash = hash ? `#${hash}` : "";

  return url.toString();
}

function consumeRedirectState(defaultServerUrl: string) {
  const fallbackServerUrl = loadActiveServer(defaultServerUrl);

  if (typeof window === "undefined") {
    return {
      serverUrl: fallbackServerUrl,
      sessionId: getStoredSessionId(fallbackServerUrl),
    };
  }

  const url = new URL(window.location.href);
  const hashParams = new URLSearchParams(url.hash.replace(/^#/, ""));
  const requestedServer = url.searchParams.get("waddle_server");
  const serverUrl = normalizeServerUrl(requestedServer || fallbackServerUrl);
  const sessionId = hashParams.get("waddle_session_id");

  if (requestedServer) {
    url.searchParams.delete("waddle_server");
    saveActiveServer(serverUrl);
  }

  if (sessionId) {
    storeSessionId(serverUrl, sessionId);
    hashParams.delete("waddle_session_id");
  }

  const nextSearch = url.searchParams.toString();
  const nextHash = hashParams.toString();
  const cleanedUrl = `${url.pathname}${nextSearch ? `?${nextSearch}` : ""}${nextHash ? `#${nextHash}` : ""}`;
  const currentUrl = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  if (cleanedUrl !== currentUrl) {
    window.history.replaceState(null, "", cleanedUrl);
  }

  return {
    serverUrl,
    sessionId: sessionId ?? getStoredSessionId(serverUrl),
  };
}

export function useAuth(defaultServerUrl: string) {
  const normalizedDefaultServerUrl = normalizeServerUrl(defaultServerUrl);
  const activeServerUrl = ref(loadActiveServer(normalizedDefaultServerUrl));
  const providers = ref<AuthProvider[]>([]);

  let auth = createServerAuth(activeServerUrl.value);

  const appState = ref<AppState>("loading");
  const session: Ref<WaddleSession | null> = ref(null);
  const api: Ref<WaddleApi | null> = ref(null);
  const appError = ref("");
  const isBootstrapping = ref(false);

  async function loadProviders() {
    try {
      providers.value = await auth.getProviders();
      appError.value = providers.value.length === 0 ? "No auth providers available on this server." : "";
    } catch (e) {
      providers.value = [];
      appError.value = e instanceof Error ? e.message : "Failed to load auth providers.";
    }
  }

  async function bootstrap() {
    isBootstrapping.value = true;
    appState.value = "loading";
    appError.value = "";

    try {
      const redirectState = consumeRedirectState(normalizedDefaultServerUrl);
      if (redirectState.serverUrl !== activeServerUrl.value) {
        activeServerUrl.value = redirectState.serverUrl;
        auth = createServerAuth(activeServerUrl.value);
      }

      const loaded = await auth.getSession(redirectState.sessionId ?? undefined);

      if (!loaded || loaded.is_expired) {
        clearStoredSessionId(activeServerUrl.value);
        session.value = null;
        api.value = null;
        // Fetch available providers for the login screen
        await loadProviders();
        appState.value = "signed-out";
        return;
      }

      saveActiveServer(activeServerUrl.value);
      storeSessionId(activeServerUrl.value, loaded.session_id);
      session.value = loaded;
      api.value = new WaddleApi(activeServerUrl.value, loaded.session_id);
      appState.value = "ready";
    } catch (e) {
      appError.value = e instanceof Error ? e.message : "Something went wrong.";
      appState.value = "error";
    } finally {
      isBootstrapping.value = false;
    }
  }

  async function login(serverUrl?: string, providerId?: string) {
    const nextServerUrl = normalizeServerUrl(serverUrl || activeServerUrl.value);
    if (nextServerUrl !== activeServerUrl.value) {
      activeServerUrl.value = nextServerUrl;
      auth = createServerAuth(nextServerUrl);
      await loadProviders();
    }

    saveActiveServer(activeServerUrl.value);
    const pid = providerId ?? providers.value[0]?.id;
    if (!pid) {
      appError.value = "No auth providers available on this server.";
      return;
    }

    const nextUrl = buildReturnUrl(
      window.location.href,
      activeServerUrl.value,
      normalizedDefaultServerUrl,
    );
    const sessionTransport =
      activeServerUrl.value === normalizedDefaultServerUrl ? "cookie" : "fragment";

    window.location.href = auth.loginUrl(nextUrl, pid, {
      sessionTransport,
    });
  }

  async function fetchProviders(serverUrl: string) {
    const nextServerUrl = normalizeServerUrl(serverUrl);
    if (nextServerUrl !== activeServerUrl.value) {
      activeServerUrl.value = nextServerUrl;
      auth = createServerAuth(nextServerUrl);
    }
    saveActiveServer(activeServerUrl.value);
    await loadProviders();
  }

  async function logout() {
    try {
      const sessionId =
        session.value?.session_id ?? getStoredSessionId(activeServerUrl.value) ?? undefined;
      await auth.logout(sessionId);
    } catch {
      // ignore logout errors
    }
    clearStoredSessionId(activeServerUrl.value);
    await loadProviders();
    session.value = null;
    api.value = null;
    appState.value = "signed-out";
    appError.value = "";
  }

  return {
    appState,
    session,
    api,
    appError,
    isBootstrapping,
    activeServerUrl,
    providers,
    bootstrap,
    login,
    fetchProviders,
    logout,
  };
}
