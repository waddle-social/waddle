<script setup lang="ts">
import { onMounted } from 'vue';

import { useTheme } from './composables/useTheme';
import { useConversations } from './composables/useConversations';
import { useRuntimeStore } from './stores/runtime';
import SidebarNav from './components/SidebarNav.vue';

useTheme();

const runtimeStore = useRuntimeStore();
const { startListening } = useConversations();

onMounted(() => {
  void runtimeStore.bootstrap();
  startListening();
});
</script>

<template>
  <div class="flex h-screen overflow-hidden bg-background">
    <!-- Left sidebar -->
    <SidebarNav />

    <!-- Main content area -->
    <main class="flex min-w-0 flex-1 flex-col bg-chat-bg">
      <RouterView />
    </main>
  </div>
</template>
