<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useRoute } from 'vue-router';

import PluginContainer from '../components/PluginContainer.vue';
import { getPluginInfo } from '../composables/usePluginLoader';
import { usePluginsStore } from '../stores/plugins';
import type { PluginInfo } from '../composables/useWaddle';

const route = useRoute();
const pluginsStore = usePluginsStore();

const pluginId = computed(() => String(route.params.id ?? ''));
const plugin = computed(() => pluginsStore.getById(pluginId.value));
const pluginInfo = ref<PluginInfo | null>(null);

const guiComponents = computed<string[]>(() => {
  if (!pluginInfo.value) return [];
  return pluginInfo.value.capabilities
    .filter((cap) => cap === 'gui-metadata')
    .length > 0 && plugin.value
    ? [`${plugin.value.name}Settings.vue`]
    : [];
});

onMounted(async () => {
  if (!pluginId.value) return;
  try {
    pluginInfo.value = await getPluginInfo(pluginId.value);
  } catch {
    // Plugin info unavailable
  }
});
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Header bar -->
    <header class="flex h-12 flex-shrink-0 items-center border-b border-border px-4 shadow-sm">
      <span class="mr-2 text-lg text-muted">🔌</span>
      <h2 class="text-base font-semibold text-foreground">Plugin</h2>
    </header>

    <div class="flex-1 overflow-y-auto p-6">
      <div class="mx-auto max-w-2xl space-y-4">
        <article v-if="plugin" class="rounded-lg bg-surface px-4 py-3">
          <h3 class="text-sm font-semibold text-foreground">{{ plugin.name }}</h3>
          <p class="mt-1 text-xs text-muted">{{ plugin.id }}</p>
          <p class="mt-2 text-sm text-foreground">
            Version {{ plugin.version }} ·
            <span class="capitalize">{{ plugin.status }}</span>
          </p>
        </article>

        <p v-else class="rounded-lg bg-surface px-4 py-3 text-sm text-muted">
          Plugin <code class="text-foreground">{{ pluginId }}</code> is not installed.
        </p>

        <PluginContainer
          v-for="componentName in guiComponents"
          :key="componentName"
          :plugin-id="pluginId"
          :component-name="componentName"
        />
      </div>
    </div>
  </div>
</template>
