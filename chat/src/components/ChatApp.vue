<script setup lang="ts">
import LandingState from "@/components/chat/LandingState.vue";
import LoginScreen from "@/components/chat/LoginScreen.vue";
import ChatReadyShell from "@/components/chat/ChatReadyShell.vue";
import { useChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  giphyApiKey?: string;
}>();

const controller = useChatAppController(props.giphyApiKey ?? "");
const { connectionStore } = controller;
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

  <ChatReadyShell
    v-else
    :controller="controller"
  />
</template>
