<script setup lang="ts">
import { inject, onMounted, onUnmounted } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import { isStaleOwnerDisposeError } from "@/lib/xmpp-client";
import { connectionStore } from "@/lib/connection-store";
import { $xmppStatus, OFFLINE_SNAPSHOT } from "@/stores/xmpp-status";
import { reportError } from "@/lib/telemetry";
import { installXmppPagehideLifecycle } from "@/lib/xmpp/pagehide-lifecycle";
import {
  isProviderLifecycleCancellation,
} from "@/lib/xmpp/provider-lifecycle";
import { createInstrumentedProviderClientCoordinator } from "@/lib/xmpp/provider-client-coordinator";
import {
  XMPP_PROVIDER_RUNTIME,
  createBrowserXmppProviderRuntime,
  type XmppProviderClient,
} from "@/lib/xmpp/xmpp-provider-runtime";
import { suspendCallForPageHide } from "@/lib/calls/call-controls";
import IncomingCallToast from "@/components/calls/IncomingCallToast.vue";
import OutgoingCallToast from "@/components/calls/OutgoingCallToast.vue";
import CallOverlay from "@/components/calls/CallOverlay.vue";
import CallErrorToast from "@/components/calls/CallErrorToast.vue";

const props = defineProps<{
  serverBaseUrl: string;
}>();

const runtime = inject(XMPP_PROVIDER_RUNTIME, null)
  ?? createBrowserXmppProviderRuntime(props.serverBaseUrl);
const auth = runtime.auth;
let disconnectPagehideLifecycle: (() => void) | null = null;

function syncStoreFromAuth() {
  connectionStore.appState = auth.appState.value;
  connectionStore.session = auth.session.value;
  connectionStore.appError = auth.appError.value;
  connectionStore.activeServerUrl = auth.activeServerUrl.value;
  connectionStore.providers = auth.providers.value;
}

function handleXmppDisposeFailure(error: unknown): void {
  // A successor may have already consumed the pagehide handoff. Its fenced
  // predecessor is expected to lose the final SM clear without touching the
  // successor. Every other disposal failure remains operationally visible.
  if (isStaleOwnerDisposeError(error)) return;
  reportError("storage.write", error, {
    recoverable: false,
    detail: "XMPP client final disposal failed",
    storage_area: "xmpp-client",
  });
}

async function disposePredecessor(client: XmppProviderClient): Promise<void> {
  try {
    await client.dispose();
  } catch (error) {
    if (isStaleOwnerDisposeError(error)) return;
    throw error;
  }
}

const providerCoordinator = createInstrumentedProviderClientCoordinator({
  getClient: runtime.getClient,
  setClient: runtime.setClient,
  createClient: runtime.createClient,
  instrumentClient: runtime.instrumentClient,
  handleStatus: runtime.handleStatus,
  disposeClient: disposePredecessor,
});

function serializeClientReplacement(
  nextSession: WaddleSession | null,
  expectedEpoch = providerCoordinator.captureActiveEpoch(),
): Promise<void> {
  return providerCoordinator.replace(nextSession, expectedEpoch);
}

async function bootstrap() {
  await providerCoordinator.bootstrap(
    async () => {
      await auth.bootstrap();
      return auth.appState.value === "ready"
        ? auth.session.value
        : null;
    },
    syncStoreFromAuth,
  );
}

connectionStore.login = async (serverUrl?: string, providerId?: string) => {
  const lifecycleEpoch = providerCoordinator.captureActiveEpoch();
  await auth.login(serverUrl, providerId);
  providerCoordinator.assertActive(lifecycleEpoch);
  syncStoreFromAuth();
};

connectionStore.fetchProviders = async (serverUrl: string) => {
  const lifecycleEpoch = providerCoordinator.captureActiveEpoch();
  await auth.fetchProviders(serverUrl);
  providerCoordinator.assertActive(lifecycleEpoch);
  syncStoreFromAuth();
};

connectionStore.logout = async () => {
  const lifecycleEpoch = providerCoordinator.captureActiveEpoch();
  let disposeFailure: unknown;
  try {
    await serializeClientReplacement(null, lifecycleEpoch);
  } catch (error) {
    disposeFailure = error;
  }
  $xmppStatus.set(OFFLINE_SNAPSHOT);
  await auth.logout();
  try {
    providerCoordinator.assertActive(lifecycleEpoch);
    syncStoreFromAuth();
  } catch (error) {
    if (!isProviderLifecycleCancellation(error)) throw error;
  }
  if (disposeFailure) throw disposeFailure;
};

connectionStore.bootstrap = bootstrap;

onMounted(() => {
  if (typeof window !== "undefined") {
    disconnectPagehideLifecycle = installXmppPagehideLifecycle(
      window,
      runtime.getClient,
      suspendCallForPageHide,
    );
  }
  void bootstrap().catch((error) => {
    if (isProviderLifecycleCancellation(error)) return;
    reportError("xmpp.stream", error, {
      recoverable: false,
      detail: "XMPP provider bootstrap failed",
    });
  });
});

onUnmounted(() => {
  disconnectPagehideLifecycle?.();
  disconnectPagehideLifecycle = null;
  void providerCoordinator.dispose().catch(handleXmppDisposeFailure);
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
