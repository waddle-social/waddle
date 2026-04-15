<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import { useAuth } from "@/composables/useAuth";
import { BrowserXmppClient } from "@/lib/xmpp-client";
import { connectionStore } from "@/lib/connection-store";

const props = defineProps<{
  serverBaseUrl: string;
}>();

const auth = useAuth(props.serverBaseUrl);

function syncStoreFromAuth() {
  connectionStore.appState = auth.appState.value;
  connectionStore.session = auth.session.value;
  connectionStore.api = auth.api.value;
  connectionStore.appError = auth.appError.value;
  connectionStore.activeServerUrl = auth.activeServerUrl.value;
  connectionStore.providers = auth.providers.value;
}

async function bootstrap() {
  await auth.bootstrap();
  syncStoreFromAuth();

  if (auth.appState.value === "ready" && auth.session.value) {
    connectionStore.client = new BrowserXmppClient(auth.session.value);
  }
}

connectionStore.login = async (serverUrl?: string, providerId?: string) => {
  await auth.login(serverUrl, providerId);
  syncStoreFromAuth();
};

connectionStore.fetchProviders = async (serverUrl: string) => {
  await auth.fetchProviders(serverUrl);
  syncStoreFromAuth();
};

connectionStore.logout = async () => {
  connectionStore.client?.disconnect().catch(() => undefined);
  connectionStore.client = null;
  await auth.logout();
  syncStoreFromAuth();
};

connectionStore.bootstrap = bootstrap;

onMounted(() => {
  void bootstrap();
});

onUnmounted(() => {
  connectionStore.client?.disconnect().catch(() => undefined);
  connectionStore.client = null;
});
</script>

<template>
  <!-- Renderless persistent island — manages XMPP connection lifecycle -->
</template>
