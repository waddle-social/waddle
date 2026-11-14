<script setup lang="ts">
import { css } from "styled-system/css";
import { panel } from "styled-system/recipes";
import AppSidebar from "@/components/community/AppSidebar.vue";
import type { ChatAppController } from "@/shell/chat-app-controller";

/**
 * The Polar Huddle shell (App-Home board): no top navigation. The sidebar
 * on the left (from `md`; on phones it opens as the navigation drawer),
 * the page in the middle (default slot) and an optional right-hand panel
 * (`#context` slot, from `xl`). `#live` and `#dock` pass through to the
 * sidebar's live-now and current-huddle areas.
 */
defineProps<{
  controller: ChatAppController;
  /** Admin bypasses the shell; settings keeps it. */
  hideSidebar?: boolean;
}>();

const rootClass = css({ display: "flex", flex: "1 1 0%", minWidth: 0, minHeight: 0, overflow: "hidden", bg: "bg", color: "fg" });
const sidebarClass = css({
  display: "none",
  md: { display: "flex", flex: "none", width: "sidebar", minHeight: 0, borderRight: "1px solid {colors.border.soft}" },
});
// Page grids size themselves to this column (`@container community-main`).
const mainClass = css({
  position: "relative",
  display: "flex",
  flex: "1 1 0%",
  flexDirection: "column",
  minWidth: 0,
  minHeight: 0,
  containerType: "inline-size",
  containerName: "community-main",
});
const contextClass = css({ display: "none", xl: { display: "flex", overflowY: "auto" } });
</script>

<template>
  <div :class="rootClass">
    <aside v-if="!hideSidebar" :class="sidebarClass">
      <AppSidebar :controller="controller">
        <template #live><slot name="live" /></template>
        <template #dock><slot name="dock" /></template>
      </AppSidebar>
    </aside>
    <main :class="mainClass">
      <slot />
    </main>
    <aside v-if="$slots.context" :class="[panel(), contextClass]" aria-label="Context">
      <slot name="context" />
    </aside>
  </div>
</template>
