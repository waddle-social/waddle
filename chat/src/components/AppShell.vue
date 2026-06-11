<script setup lang="ts">
import { onBeforeUnmount } from "vue";
import LandingState from "@/components/chat/LandingState.vue";
import LoginScreen from "@/components/chat/LoginScreen.vue";
import CallAudioSink from "@/components/calls/CallAudioSink.vue";
import { useChatAppController } from "@/shell/chat-app-controller";
import { appController } from "@/stores/app-controller";

const props = defineProps<{
  giphyApiKey?: string;
}>();

const controller = useChatAppController(props.giphyApiKey ?? "");
const { connectionStore } = controller;

// Publish the controller into a module-level ref so per-route Vue
// islands mounted as siblings (Phase 3) can read shared state without
// prop drilling. AppShell is the only writer.
appController.value = controller;

onBeforeUnmount(() => {
  // Defensive: if AppShell ever unmounts (it's `transition:persist` in
  // AppLayout, so this should be rare), drop the global handle so a
  // stale controller can't accidentally outlive its setup scope.
  appController.value = null;
});
</script>

<template>
  <LandingState
    v-if="connectionStore.appState === 'loading'"
    title="Checking session."
  />

  <LoginScreen
    v-else-if="connectionStore.appState === 'signed-out'"
    :default-server-url="connectionStore.activeServerUrl"
    :active-server-url="connectionStore.activeServerUrl"
    :providers="connectionStore.providers"
    :error-message="connectionStore.appError"
    @login="(url, pid) => connectionStore.login(url, pid)"
    @fetch-providers="connectionStore.fetchProviders"
  />

  <LandingState
    v-else-if="connectionStore.appState === 'error'"
    title="Server unavailable."
    :copy="connectionStore.appError"
    action-label="Try again"
    @action="connectionStore.bootstrap"
  />

  <!--
    Ready: render the per-route Vue page mounted by the Astro page into
    AppLayout's slot. Each per-route page reads `appController` from the
    module-level store to access shared connection / channel / unread
    state without prop drilling across separate Astro islands.
  -->
  <template v-else>
    <CallAudioSink />
    <slot />
  </template>
</template>
