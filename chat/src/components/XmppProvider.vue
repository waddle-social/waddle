<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import { useSessionAuth } from "@/auth/session";
import { BrowserXmppClient } from "@/lib/xmpp-client";
import { createXmppBootstrapCoordinator } from "@/lib/xmpp/bootstrap-coordinator";
import { connectionStore } from "@/lib/connection-store";
import { $xmppStatus, OFFLINE_SNAPSHOT } from "@/stores/xmpp-status";
import { installInstrumentation } from "@/lib/xmpp/xmpp-instrumentation";
import { installXmppPagehideLifecycle } from "@/lib/xmpp/pagehide-lifecycle";
import { suspendCallForPageHide } from "@/lib/calls/call-controls";
import { reportXmppPageLifecycleFailure } from "@/lib/telemetry";
import IncomingCallToast from "@/components/calls/IncomingCallToast.vue";
import OutgoingCallToast from "@/components/calls/OutgoingCallToast.vue";
import CallOverlay from "@/components/calls/CallOverlay.vue";
import CallErrorToast from "@/components/calls/CallErrorToast.vue";

const props = defineProps<{
  serverBaseUrl: string;
}>();

const auth = useSessionAuth(props.serverBaseUrl);
let disposePageLifecycle: (() => void) | null = null;
const bootstrapCoordinator = createXmppBootstrapCoordinator(
  () => connectionStore.client,
  (client) => { connectionStore.client = client; },
);

function syncStoreFromAuth() {
  connectionStore.appState = auth.appState.value;
  connectionStore.session = auth.session.value;
  connectionStore.appError = auth.appError.value;
  connectionStore.activeServerUrl = auth.activeServerUrl.value;
  connectionStore.providers = auth.providers.value;
}

async function bootstrap() {
  const generation = bootstrapCoordinator.begin();
  await auth.bootstrap();
  if (!bootstrapCoordinator.isCurrent(generation)) return;
  syncStoreFromAuth();

  if (auth.appState.value === "ready" && auth.session.value) {
    await bootstrapCoordinator.replace(generation, () => {
      const client = new BrowserXmppClient(auth.session.value!);
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
        if (connectionStore.client !== client) return;
        $xmppStatus.set(status);
      });
      return client;
    });
    return;
  }

  // A refreshed auth bootstrap can revoke or expire a previously-ready
  // session without unmounting this provider. Never leave that old socket or
  // its status handler alive after publishing the non-ready auth state.
  if (bootstrapCoordinator.detachIfCurrent(generation)) {
    $xmppStatus.set(OFFLINE_SNAPSHOT);
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
  // Logical ownership releases before the first await in `dispose`; retain
  // best-effort physical close without letting a stalled socket delay logout.
  bootstrapCoordinator.detach();
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
      reportXmppPageLifecycleFailure,
    );
  }
  void bootstrap();
});

onUnmounted(() => {
  bootstrapCoordinator.detach();
  disposePageLifecycle?.();
  disposePageLifecycle = null;
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
