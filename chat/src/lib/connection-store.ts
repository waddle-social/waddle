import { shallowReactive } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession, AuthProvider } from "@/lib/server-auth";
import type { AppState } from "@/lib/chat-ui";

interface ConnectionStore {
  client: BrowserXmppClient | null;
  appState: AppState;
  session: WaddleSession | null;
  api: null;
  appError: string;
  activeServerUrl: string;
  providers: AuthProvider[];
  login: (serverUrl?: string, providerId?: string) => Promise<void>;
  logout: () => Promise<void>;
  fetchProviders: (serverUrl: string) => Promise<void>;
  bootstrap: () => Promise<void>;
}

export const connectionStore: ConnectionStore = shallowReactive({
  client: null,
  appState: "loading" as AppState,
  session: null as WaddleSession | null,
  api: null,
  appError: "",
  activeServerUrl: "",
  providers: [] as AuthProvider[],
  login: async () => {},
  logout: async () => {},
  fetchProviders: async () => {},
  bootstrap: async () => {},
});
