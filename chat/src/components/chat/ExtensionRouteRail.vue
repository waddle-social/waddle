<script setup lang="ts">
import { computed } from "vue";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import { extensionRouteIconComponent, extensionRouteRailItems } from "./extension-route-rail-model";

const props = defineProps<{
  routes: DiscoveredExtensionRoute[];
  activeRoute?: { pluginId: string; routeId: string } | null;
  active?: boolean;
}>();

const emit = defineEmits<{
  selectRoute: [route: DiscoveredExtensionRoute];
}>();

const items = computed(() => extensionRouteRailItems(props.routes, props.activeRoute, !!props.active));
// A slim icon rail beside the timeline (lg and up); the open route is `aria-current`.
const railClass = css({
  display: "none",
  lg: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "1",
    width: "56px",
    minHeight: 0,
    flex: "none",
    overflowX: "hidden",
    overflowY: "auto",
    py: "2",
    borderLeft: "1px solid {colors.border.soft}",
    bg: "bg.rail",
  },
});
const buttonClass = css({ "&[aria-current=page]": { bg: "state.selected", color: "fg", boxShadow: "inset 0 0 0 1px {colors.border.strong}" } });
</script>

<template>
  <aside
    v-if="routes.length > 0"
    :class="railClass"
    aria-label="Channel extension routes"
  >
    <button
      v-for="item in items"
      :key="item.key"
      type="button"
      :class="[iconbtn(), buttonClass]"
      :title="item.label"
      :aria-label="item.label"
      :aria-current="item.isActive ? 'page' : undefined"
      @click="emit('selectRoute', item.route)"
    >
      <component :is="extensionRouteIconComponent(item.icon)" aria-hidden="true" />
    </button>
  </aside>
</template>
