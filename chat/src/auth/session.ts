import { ref, type Ref } from "vue";
import { createServerAuth, type WaddleSession, type AuthProvider } from "@/lib/server-auth";
import type { AppState } from "@/lib/chat-ui";
import { normalizeHomeServer } from "@/auth/home-server";
import {
  beginLoginRedirect,
  consumeRedirectSession,
  readSessionSnapshot,
  replaceSession,
} from "@/auth/redirect-session";

export function useSessionAuth(configuredServerUrl: string) {
  const defaultServerUrl = normalizeHomeServer(configuredServerUrl) ?? "";
  const activeServerUrl = ref(defaultServerUrl);
  const providers = ref<AuthProvider[]>([]);
  const appState = ref<AppState>("loading");
  const session: Ref<WaddleSession | null> = ref(null);
  const appError = ref("");
  const isBootstrapping = ref(false);
  let providerGeneration = 0;
  let sessionGeneration = 0;
  let providerServer = "";
  let authenticatedBinding: { serverUrl: string; sessionId: string } | null = null;

  function invalidateProviders() {
    providerGeneration += 1;
    providerServer = "";
    providers.value = [];
    appError.value = "";
  }

  async function loadProviders(value: string) {
    invalidateProviders();
    const generation = providerGeneration;
    const serverUrl = normalizeHomeServer(value);
    if (!serverUrl) {
      appError.value = "Enter a valid HTTPS homeserver URL.";
      return;
    }
    activeServerUrl.value = serverUrl;
    try {
      const loaded = await createServerAuth(serverUrl).getProviders();
      if (generation !== providerGeneration) return;
      providers.value = loaded;
      providerServer = serverUrl;
      appError.value = loaded.length === 0 ? "No auth providers available on this server." : "";
    } catch (error) {
      if (generation !== providerGeneration) return;
      appError.value = error instanceof Error ? error.message : "Failed to load auth providers.";
    }
  }

  async function bootstrap() {
    const generation = ++sessionGeneration;
    invalidateProviders();
    isBootstrapping.value = true;
    appState.value = "loading";
    appError.value = "";
    try {
      await consumeRedirectSession();
      const snapshot = await readSessionSnapshot();
      if (generation !== sessionGeneration) return;
      const serverUrl = snapshot.session?.serverUrl ?? defaultServerUrl;
      if (!serverUrl) throw new Error("No valid homeserver is configured.");
      activeServerUrl.value = serverUrl;
      // A tombstone records an explicit logout. Do not revive it from a
      // cookie that may remain valid after a failed or concurrent logout.
      if (!snapshot.session && snapshot.revision !== null) {
        authenticatedBinding = null;
        session.value = null;
        await loadProviders(serverUrl);
        if (generation === sessionGeneration) appState.value = "signed-out";
        return;
      }
      const loaded = await createServerAuth(serverUrl).getSession(snapshot.session?.sessionId);
      if (generation !== sessionGeneration) return;
      const current = await readSessionSnapshot();
      if (generation !== sessionGeneration) return;
      if (current.revision !== snapshot.revision) return bootstrap();
      if (!loaded || loaded.is_expired) {
        if (snapshot.session && !await replaceSession(snapshot, null)) return bootstrap();
        if (generation !== sessionGeneration) return;
        authenticatedBinding = null;
        session.value = null;
        await loadProviders(serverUrl);
        if (generation === sessionGeneration) appState.value = "signed-out";
        return;
      }
      if (!snapshot.session || snapshot.session.sessionId !== loaded.session_id) {
        if (!await replaceSession(snapshot, { serverUrl, sessionId: loaded.session_id })) return bootstrap();
      }
      if (generation !== sessionGeneration) return;
      authenticatedBinding = { serverUrl, sessionId: loaded.session_id };
      session.value = loaded;
      appState.value = "ready";
    } catch (error) {
      if (generation !== sessionGeneration) return;
      authenticatedBinding = null;
      session.value = null;
      appError.value = error instanceof Error ? error.message : "Something went wrong.";
      appState.value = "error";
    } finally {
      if (generation === sessionGeneration) isBootstrapping.value = false;
    }
  }

  async function login(value?: string, providerId?: string) {
    const serverUrl = normalizeHomeServer(value ?? activeServerUrl.value);
    const pid = providerId ?? providers.value[0]?.id;
    if (!serverUrl || providerServer !== serverUrl || !pid || !providers.value.some((provider) => provider.id === pid)) {
      appError.value = "Select a homeserver and wait for its sign-in methods to load.";
      return;
    }
    const generation = providerGeneration;
    const nextUrl = new URL(window.location.href);
    nextUrl.searchParams.delete("waddle_server");
    const hashParams = new URLSearchParams(nextUrl.hash.replace(/^#/, ""));
    hashParams.delete("waddle_session_id");
    const hash = hashParams.toString();
    nextUrl.hash = hash ? `#${hash}` : "";
    if (!await beginLoginRedirect(serverUrl, nextUrl)) {
      appError.value = "Cannot start sign-in because browser storage is unavailable.";
      return;
    }
    if (generation !== providerGeneration || providerServer !== serverUrl) return;
    window.location.href = createServerAuth(serverUrl).loginUrl(nextUrl.toString(), pid, { sessionTransport: "fragment" });
  }

  async function fetchProviders(serverUrl: string) {
    await loadProviders(serverUrl);
  }

  async function logout() {
    const generation = ++sessionGeneration;
    invalidateProviders();
    try {
      const snapshot = await readSessionSnapshot();
      if (generation !== sessionGeneration) return;
      if (authenticatedBinding && (snapshot.session?.serverUrl !== authenticatedBinding.serverUrl
        || snapshot.session.sessionId !== authenticatedBinding.sessionId)) return bootstrap();
      if (snapshot.session) {
        try {
          await createServerAuth(snapshot.session.serverUrl).logout(snapshot.session.sessionId ?? undefined);
        } catch {
          // Logout remains best effort; local state must still be retired.
        }
      }
      if (generation !== sessionGeneration) return;
      if (!await replaceSession(snapshot, null)) return bootstrap();
      if (generation !== sessionGeneration) return;
      authenticatedBinding = null;
      session.value = null;
      await loadProviders(activeServerUrl.value || defaultServerUrl);
      if (generation === sessionGeneration) {
        appState.value = "signed-out";
        appError.value = "";
      }
    } catch (error) {
      if (generation !== sessionGeneration) return;
      authenticatedBinding = null;
      session.value = null;
      appError.value = error instanceof Error ? error.message : "Cannot clear the session.";
      appState.value = "error";
    }
  }

  return {
    appState, session, appError, isBootstrapping, defaultServerUrl, activeServerUrl,
    providers, bootstrap, login, fetchProviders, invalidateProviders, logout,
  };
}
