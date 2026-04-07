import { ref, type Ref } from "vue";
import { createServerAuth, type WaddleSession } from "@/lib/server-auth";
import { WaddleApi } from "@/lib/waddle-api";
import type { AppState } from "@/lib/chat-ui";

export function useAuth(serverBaseUrl: string) {
  const auth = createServerAuth(serverBaseUrl);

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
        appState.value = "signed-out";
        return;
      }

      session.value = loaded;
      api.value = new WaddleApi(serverBaseUrl, loaded.session_id);
      appState.value = "ready";
    } catch (e) {
      appError.value = e instanceof Error ? e.message : "Something went wrong.";
      appState.value = "error";
    } finally {
      isBootstrapping.value = false;
    }
  }

  function login() {
    window.location.href = auth.loginUrl(window.location.href);
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
    bootstrap,
    login,
    logout,
  };
}
