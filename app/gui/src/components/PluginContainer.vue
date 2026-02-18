<script setup lang="ts">
import { computed, onErrorCaptured, ref, type Component } from 'vue';

import { loadPluginComponent } from '../composables/usePluginLoader';

const props = defineProps<{
  pluginId: string;
  componentName: string;
  pluginProps?: Record<string, unknown>;
}>();

const emit = defineEmits<{
  error: [error: Error];
}>();

const renderError = ref<string | null>(null);

const pluginComponent = computed<Component | null>(() => {
  if (!props.pluginId || !props.componentName) {
    return null;
  }
  return loadPluginComponent(props.pluginId, props.componentName);
});

onErrorCaptured((error: Error) => {
  renderError.value = error.message;
  emit('error', error);
  return false;
});
</script>

<template>
  <div class="plugin-container">
    <div v-if="renderError" class="rounded-xl border border-danger bg-surface p-4 text-sm">
      <p class="font-semibold text-danger">Plugin Error</p>
      <p class="mt-1 text-muted">{{ pluginId }}: {{ renderError }}</p>
    </div>
    <Suspense v-else-if="pluginComponent">
      <component :is="pluginComponent" v-bind="pluginProps" />
      <template #fallback>
        <div class="flex items-center gap-2 p-4 text-sm text-muted">Loading plugin...</div>
      </template>
    </Suspense>
    <div v-else class="p-4 text-sm text-muted">No component available.</div>
  </div>
</template>
