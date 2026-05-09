<script setup lang="ts">
import { Grid3X3, Link2, ListChecks } from "lucide-vue-next";
import { computed } from "vue";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import { extensionRouteRailItems, type ExtensionRouteRailIcon } from "./extension-route-rail-model";

const props = defineProps<{
  routes: DiscoveredExtensionRoute[];
  activeRoute?: { pluginId: string; routeId: string } | null;
  active?: boolean;
}>();

const emit = defineEmits<{
  selectRoute: [route: DiscoveredExtensionRoute];
}>();

const items = computed(() => extensionRouteRailItems(props.routes, props.activeRoute, !!props.active));

function routeIcon(icon: ExtensionRouteRailIcon) {
  if (icon === "links") return Link2;
  if (icon === "gallery") return Grid3X3;
  return ListChecks;
}
</script>

<template>
  <aside
    v-if="routes.length > 0"
    class="extension-route-rail"
    aria-label="Channel extension routes"
  >
    <button
      v-for="item in items"
      :key="item.key"
      type="button"
      class="extension-route-rail__button"
      :class="item.isActive ? 'extension-route-rail__button--active' : ''"
      :title="item.label"
      :aria-label="item.label"
      :aria-current="item.isActive ? 'page' : undefined"
      @click="emit('selectRoute', item.route)"
    >
      <component :is="routeIcon(item.icon)" class="h-4 w-4" aria-hidden="true" />
    </button>
  </aside>
</template>
