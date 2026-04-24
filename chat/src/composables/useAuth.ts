import { ref, type Ref } from "vue";
import { createServerAuth, type WaddleSession, type AuthProvider } from "@/lib/server-auth";
import type { AppState } from "@/lib/chat-ui";

const SESSION_STORAGE_KEY = "waddle.chat.session";

function trimTrailingSlash(value: string) {
  return value.endsWith("/") ? value.slice(0, -1) : value;
}

function normalizeServerUrl(value: string) {
  return trimTrailingSlash(value.trim());
}

function getStoredSessionId() {
  if (typeof window === "undefined") return null;

  try {
    return window.localStorage.getItem(SESSION_STORAGE_KEY);
  } catch {
    return null;
  }
}

function storeSessionId(sessionId: string) {
  if (typeof window === "undefined") return;

  try {
    window.localStorage.setItem(SESSION_STORAGE_KEY, sessionId);
  } catch {
    // ignore storage failures
  }
}

function clearStoredSessionId() {
  if (typeof window === "undefined") return;

  try {
    window.localStorage.removeItem(SESSION_STORAGE_KEY);
  } catch {
    // ignore storage failures
  }
}

function consumeRedirectSession() {
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

export function useAuth(defaultServerUrl: string) {
  const activeServerUrl = ref(normalizeServerUrl(defaultServerUrl));
  const providers = ref<AuthProvider[]>([]);
  const auth = createServerAuth(activeServerUrl.value);

  const appState = ref<AppState>("loading");
  const session: Ref<WaddleSession | null> = ref(null);
  const api: Ref<null> = ref(null);
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
      const loaded = await auth.getSession(consumeRedirectSession() ?? undefined);

      if (!loaded || loaded.is_expired) {
        clearStoredSessionId();
        session.value = null;
        api.value = null;
        await loadProviders();
        appState.value = "signed-out";
        return;
      }

      storeSessionId(loaded.session_id);
      session.value = loaded;
      api.value = null;
      appState.value = "ready";
    } catch (e) {
      appError.value = e instanceof Error ? e.message : "Something went wrong.";
      appState.value = "error";
    } finally {
      isBootstrapping.value = false;
    }
  }

  async function login(_serverUrl?: string, providerId?: string) {
    const pid = providerId ?? providers.value[0]?.id;
    if (!pid) {
      appError.value = "No auth providers available on this server.";
      return;
    }

    const nextUrl = new URL(window.location.href);
    nextUrl.searchParams.delete("waddle_server");
    const hashParams = new URLSearchParams(nextUrl.hash.replace(/^#/, ""));
    hashParams.delete("waddle_session_id");
    const hash = hashParams.toString();
    nextUrl.hash = hash ? `#${hash}` : "";

    const pageOrigin = window.location.origin;
    const serverOrigin = new URL(activeServerUrl.value).origin;
    const sessionTransport = pageOrigin === serverOrigin ? "cookie" : "fragment";

    window.location.href = auth.loginUrl(nextUrl.toString(), pid, {
      sessionTransport,
    });
  }

  async function fetchProviders(_serverUrl?: string) {
    await loadProviders();
  }

  async function logout() {
    try {
      await auth.logout(session.value?.session_id ?? getStoredSessionId() ?? undefined);
    } catch {
      // ignore logout errors
    }
    clearStoredSessionId();
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
