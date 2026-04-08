import { ref, type Ref } from "vue";
import { createServerAuth, type WaddleSession, type AuthProvider } from "@/lib/server-auth";
import { WaddleApi } from "@/lib/waddle-api";
import type { AppState } from "@/lib/chat-ui";

export function useAuth(defaultServerUrl: string) {
  const activeServerUrl = ref(defaultServerUrl);
  const providers = ref<AuthProvider[]>([]);

  let auth = createServerAuth(activeServerUrl.value);

  const appState = ref<AppState>("loading");
  const session: Ref<WaddleSession | null> = ref(null);
  const api: Ref<WaddleApi | null> = ref(null);
  const appError = ref("");
  const isBootstrapping = ref(false);

  async function bootstrap() {
    isBootstrapping.value = true;
    appState.value = "loading";
    appError.value = "";

    try {
      const loaded = await auth.getSession();

      if (!loaded || loaded.is_expired) {
        session.value = null;
        api.value = null;
        // Fetch available providers for the login screen
        providers.value = await auth.getProviders();
        appState.value = "signed-out";
        return;
      }

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
    if (serverUrl && serverUrl !== activeServerUrl.value) {
      activeServerUrl.value = serverUrl;
      auth = createServerAuth(serverUrl);
      // Re-fetch providers for new server
      providers.value = await auth.getProviders();
    }

    // Use provided ID, or first available provider, or omit
    const pid = providerId ?? providers.value[0]?.id;
    window.location.href = auth.loginUrl(window.location.href, pid);
  }

  async function fetchProviders(serverUrl: string) {
    if (serverUrl !== activeServerUrl.value) {
      activeServerUrl.value = serverUrl;
      auth = createServerAuth(serverUrl);
    }
    providers.value = await auth.getProviders();
  }

  async function logout() {
    try {
      await auth.logout();
    } catch {
      // ignore logout errors
    }
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
