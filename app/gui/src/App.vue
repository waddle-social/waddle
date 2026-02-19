<script setup lang="ts">
import { computed, watch } from 'vue';
import { useRoute } from 'vue-router';

import { useTheme } from './composables/useTheme';
import { useConversations } from './composables/useConversations';
import { useRuntimeStore } from './stores/runtime';
import { useAuthStore } from './stores/auth';
import SidebarNav from './components/SidebarNav.vue';

useTheme();

const route = useRoute();
const runtimeStore = useRuntimeStore();
const authStore = useAuthStore();
const { setActiveConversation, startListening, stopListening } = useConversations();

const isLoginRoute = computed(() => route.name === 'login');

function decodeRouteJid(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

// Only bootstrap runtime & conversations when authenticated
watch(
  () => authStore.isAuthenticated,
  (authed) => {
    if (authed) {
      void runtimeStore.bootstrap();
      startListening();
    } else {
      runtimeStore.shutdown();
      stopListening();
      setActiveConversation(null);
    }
  },
  { immediate: true },
);

watch(
  () => [authStore.isAuthenticated, route.name, route.params.jid] as const,
  ([authed, routeName, routeJid]) => {
    if (!authed || routeName !== 'chat') {
      setActiveConversation(null);
      return;
    }
    setActiveConversation(decodeRouteJid(String(routeJid ?? '')));
  },
  { immediate: true },
);
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-background">
    <!-- Left sidebar (hidden on login) -->
    <SidebarNav v-if="!isLoginRoute" />

    <!-- Main content area -->
    <main class="flex min-w-0 flex-1 flex-col bg-chat-bg">
      <RouterView />
    </main>
  </div>
</template>
