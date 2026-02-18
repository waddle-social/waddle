<script setup lang="ts">
import { computed, onMounted, watch } from 'vue';
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
const { startListening, stopListening } = useConversations();

const isLoginRoute = computed(() => route.name === 'login');

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
    }
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
