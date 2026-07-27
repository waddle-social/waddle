<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import { useSessionAuth } from "@/auth/session";
import { BrowserXmppClient } from "@/lib/xmpp-client";
import { connectionStore } from "@/lib/connection-store";
import { $xmppStatus, OFFLINE_SNAPSHOT } from "@/stores/xmpp-status";
import { installInstrumentation } from "@/lib/xmpp/xmpp-instrumentation";
import { installXmppPagehideLifecycle } from "@/lib/xmpp/pagehide-lifecycle";
import { suspendCallForPageHide } from "@/lib/calls/call-controls";
import IncomingCallToast from "@/components/calls/IncomingCallToast.vue";
import OutgoingCallToast from "@/components/calls/OutgoingCallToast.vue";
import CallOverlay from "@/components/calls/CallOverlay.vue";
import CallErrorToast from "@/components/calls/CallErrorToast.vue";

const props = defineProps<{
  serverBaseUrl: string;
}>();

const auth = useSessionAuth(props.serverBaseUrl);
let disposePageLifecycle: (() => void) | null = null;

function syncStoreFromAuth() {
  connectionStore.appState = auth.appState.value;
  connectionStore.session = auth.session.value;
  connectionStore.appError = auth.appError.value;
  connectionStore.activeServerUrl = auth.activeServerUrl.value;
  connectionStore.providers = auth.providers.value;
}

async function bootstrap() {
  await auth.bootstrap();
  syncStoreFromAuth();

  if (auth.appState.value === "ready" && auth.session.value) {
    const client = new BrowserXmppClient(auth.session.value);
    // Wire Faro telemetry hooks before any handlers are set — the
    // primary setStatusHandler below is unaffected, but the telemetry
    // `onStatus` hook fires alongside it. Safe to install even when
    // Faro isn't initialized; `@/lib/telemetry` no-ops until bootstrap.
    installInstrumentation(client);
    // The status handler is owned here — XmppProvider is persisted across
    // route changes (transition:persist), so there is exactly one writer to
    // $xmppStatus for the lifetime of the client. Transient consumers
    // (useChannelMessages, connection-notice) read the store via useStore.
    client.setStatusHandler((status) => {
      $xmppStatus.set(status);
    });
    connectionStore.client = client;
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
  $xmppStatus.set(OFFLINE_SNAPSHOT);
  await auth.logout();
  syncStoreFromAuth();
};

connectionStore.bootstrap = bootstrap;

onMounted(() => {
  if (typeof window !== "undefined") {
    disposePageLifecycle = installXmppPagehideLifecycle(
      window,
      () => connectionStore.client,
      suspendCallForPageHide,
    );
  }
  void bootstrap();
});

onUnmounted(() => {
  disposePageLifecycle?.();
  disposePageLifecycle = null;
  connectionStore.client?.disconnect().catch(() => undefined);
  connectionStore.client = null;
  $xmppStatus.set(OFFLINE_SNAPSHOT);
});
</script>

<template>
  <!-- Persistent island — manages XMPP connection lifecycle plus the
       global call surfaces (incoming-call toast, outgoing-ring +
       ended toast, in-call overlay). -->
  <IncomingCallToast />
  <OutgoingCallToast />
  <CallOverlay />
  <!-- Renders only when phase is idle/ended; covers pre-transition
       call errors that the other three surfaces never see. -->
  <CallErrorToast />
</template>
