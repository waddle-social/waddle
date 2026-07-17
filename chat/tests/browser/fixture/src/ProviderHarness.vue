<script setup lang="ts">
import { provide, ref } from "vue";
import XmppProvider from "@/components/XmppProvider.vue";
import type { AppState } from "@/lib/chat-ui";
import { connectionStore } from "@/lib/connection-store";
import type { AuthProvider, WaddleSession } from "@/lib/server-auth";
import type { XmppStatusSnapshot } from "@/lib/xmpp-client";
import {
  XMPP_PROVIDER_RUNTIME,
  type XmppProviderClient,
  type XmppProviderRuntime,
} from "@/lib/xmpp/xmpp-provider-runtime";
import type { ProviderBrowserFixture } from "./provider-fixture-types";

const FIRST_SESSION: WaddleSession = {
  session_id: "bootstrap-session",
  username: "bootstrap-user",
  avatar_url: null,
  xmpp_localpart: "bootstrap-user",
  jid: "bootstrap-user@example.com",
  xmpp_websocket_url: "wss://example.com/ws",
  is_expired: false,
  expires_at: null,
};
const LOGIN_SESSION: WaddleSession = {
  ...FIRST_SESSION,
  session_id: "login-session",
  username: "login-user",
  xmpp_localpart: "login-user",
  jid: "login-user@example.com",
};
const SECOND_SESSION: WaddleSession = {
  ...FIRST_SESSION,
  session_id: "second-bootstrap-session",
  username: "second-bootstrap-user",
  xmpp_localpart: "second-bootstrap-user",
  jid: "second-bootstrap-user@example.com",
};

class FixtureClient implements XmppProviderClient {
  disposeCalls = 0;

  constructor(
    readonly id: string,
    private readonly events: string[],
  ) {}

  async dispose(): Promise<void> {
    this.disposeCalls += 1;
    this.events.push(`dispose:${this.id}`);
  }

  setStatusHandler(_handler: (status: XmppStatusSnapshot) => void): void {
    this.events.push(`status-handler:${this.id}`);
  }

  prepareForPageHide(): void {}

  resumeAfterPageShow(): void {}
}

const events: string[] = [];
const clients = new Map<string, FixtureClient>();
let activeClient: FixtureClient | null = null;
let bootstrapCount = 0;

const appState = ref<AppState>("loading");
const session = ref<WaddleSession | null>(null);
const appError = ref("");
const isBootstrapping = ref(false);
const activeServerUrl = ref("https://bootstrap.example.com");
const providers = ref<AuthProvider[]>([]);

const runtime: XmppProviderRuntime = {
  auth: {
    appState,
    session,
    appError,
    isBootstrapping,
    activeServerUrl,
    providers,
    async bootstrap() {
      bootstrapCount += 1;
      isBootstrapping.value = true;
      session.value = bootstrapCount === 1 ? FIRST_SESSION : SECOND_SESSION;
      appState.value = "ready";
      appError.value = `bootstrap:${bootstrapCount}`;
      providers.value = [{ id: `bootstrap-provider-${bootstrapCount}`, kind: "oidc" }];
      isBootstrapping.value = false;
      events.push(`auth:bootstrap:${bootstrapCount}`);
    },
    async login(serverUrl?: string, providerId?: string) {
      session.value = LOGIN_SESSION;
      appState.value = "ready";
      activeServerUrl.value = serverUrl ?? activeServerUrl.value;
      appError.value = providerId ?? "";
      providers.value = [{ id: "login-provider", kind: "oidc" }];
      events.push("auth:login");
    },
    async fetchProviders(serverUrl?: string) {
      activeServerUrl.value = serverUrl ?? activeServerUrl.value;
      providers.value = [{ id: "fetched-provider", kind: "oidc" }];
      events.push("auth:fetch-providers");
    },
    async logout() {
      session.value = null;
      appState.value = "signed-out";
      appError.value = "";
      providers.value = [{ id: "signed-out-provider", kind: "oidc" }];
      events.push("auth:logout");
    },
  },
  getClient: () => activeClient,
  setClient: (client) => {
    if (client !== null && !(client instanceof FixtureClient)) {
      throw new TypeError("provider fixture received a foreign client");
    }
    activeClient = client;
    events.push(`set:${activeClient?.id ?? "null"}`);
  },
  createClient: (nextSession) => {
    const client = new FixtureClient(nextSession.session_id, events);
    clients.set(client.id, client);
    events.push(`create:${client.id}`);
    return client;
  },
  instrumentClient: (client) => {
    if (!(client instanceof FixtureClient)) {
      throw new TypeError("provider fixture cannot instrument a foreign client");
    }
    events.push(`instrument:${client.id}`);
  },
  handleStatus: (status) => {
    events.push(`status:${status.state}`);
  },
};

provide(XMPP_PROVIDER_RUNTIME, runtime);

window.__waddleProviderFixture = {
  snapshot: () => ({
    appState: connectionStore.appState,
    sessionId: connectionStore.session?.session_id ?? null,
    appError: connectionStore.appError,
    activeServerUrl: connectionStore.activeServerUrl,
    providerIds: connectionStore.providers.map(({ id }) => id),
    activeClientId: activeClient?.id ?? null,
    disposeCalls: Object.fromEntries(
      [...clients].map(([id, client]) => [id, client.disposeCalls]),
    ),
    events: [...events],
  }),
  login: () => connectionStore.login(
    "https://login.example.com",
    "selected-provider",
  ),
  logout: () => connectionStore.logout(),
  bootstrap: () => connectionStore.bootstrap(),
  unmount: () => {
    throw new Error("Provider fixture mount has not published unmount");
  },
} satisfies ProviderBrowserFixture;
</script>

<template>
  <XmppProvider server-base-url="https://bootstrap.example.com" />
</template>
