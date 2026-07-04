import { ref, type Ref } from "vue";
import { createServerAuth, type WaddleSession, type AuthProvider } from "@/lib/server-auth";
import type { AppState } from "@/lib/chat-ui";
import {
  clearStoredSessionId,
  consumeRedirectSession,
  getStoredSessionId,
  storeSessionId,
} from "@/auth/redirect-session";

function trimTrailingSlash(value: string) {
  return value.endsWith("/") ? value.slice(0, -1) : value;
}

function normalizeServerUrl(value: string) {
  return trimTrailingSlash(value.trim());
}

export function useSessionAuth(defaultServerUrl: string) {
  const activeServerUrl = ref(normalizeServerUrl(defaultServerUrl));
  const providers = ref<AuthProvider[]>([]);
  const auth = createServerAuth(activeServerUrl.value);

  const appState = ref<AppState>("loading");
  const session: Ref<WaddleSession | null> = ref(null);
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
        await loadProviders();
        appState.value = "signed-out";
        return;
      }

      storeSessionId(loaded.session_id);
      session.value = loaded;
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
    appState.value = "signed-out";
    appError.value = "";
  }

  return {
    appState,
    session,
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
