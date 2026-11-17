<script setup lang="ts">
// Admin shell (App-Admin / Mobile-Admin boards): one column with the
// "Admin" heading, a per-panel caption, the panel's New action (teleported
// into `#admin-header-actions` by the panel) and Users / Spaces / Channels
// as one segmented switch, inside the sidebar shell. Below `md` a top bar
// with a drawer (and Back to chat) replaces the heading and the switch.
import { computed, ref } from "vue";
import { Menu, X } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { empty, iconbtn, seg } from "styled-system/recipes";
import spotError from "@waddle/design/assets/spot-error.svg?url";
import AppDrawer from "@/components/ui/AppDrawer.vue";
import type { AdminMatch, AdminPanel } from "@/router";

interface PanelDef {
  slug: AdminPanel;
  label: string;
  caption: string;
  enabled: boolean;
}

const props = defineProps<{
  /** Slug of the currently rendered panel. */
  activePanel: AdminPanel;
}>();

const emit = defineEmits<{
  navigate: [match: AdminMatch];
  back: [];
}>();

// Audit log, Push health and Settings are stubs: reachable by URL only,
// left out of the switch (Admin product note).
const PANELS: ReadonlyArray<PanelDef> = [
  { slug: "users", label: "Users", caption: "Every account on this server.", enabled: true },
  { slug: "spaces", label: "Spaces", caption: "Deleting a space also deletes its channels.", enabled: true },
  { slug: "channels", label: "Channels", caption: "Open a channel to edit its settings, roles and occupants.", enabled: true },
  { slug: "audit", label: "Audit log", caption: "Server activity, once it ships.", enabled: false },
  { slug: "push-health", label: "Push health", caption: "Push delivery health, once it ships.", enabled: false },
  { slug: "settings", label: "Settings", caption: "Server settings, once they ship.", enabled: false },
];
const NAV = PANELS.filter((panel) => panel.enabled);

const sidebarOpen = ref(false);

function onPanelClick(panel: PanelDef): void {
  sidebarOpen.value = false;
  emit("navigate", { id: "admin", params: { panel: panel.slug } });
}

const active = computed(() => PANELS.find((p) => p.slug === props.activePanel));
const activeLabel = computed(() => active.value?.label ?? "Admin");
const isStub = computed(() => !active.value || !active.value.enabled);

const rootClass = css({ flex: 1, minHeight: 0, overflowY: "auto", bg: "bg", color: "fg" });
const topBarClass = css({
  position: "sticky",
  top: 0,
  zIndex: "sticky",
  display: "flex",
  alignItems: "center",
  gap: "1.5",
  padding: "2",
  borderBottom: "1px solid {colors.border.soft}",
  bg: "bg.rail",
  color: "fg",
  md: { display: "none" },
  "& b": { flex: 1, minWidth: 0, font: "700 18px/1.3 {fonts.display}" },
});
const bigIconClass = cx(iconbtn(), css({ width: "44px", height: "44px" }));
const mainClass = css({
  width: "100%",
  maxWidth: "1000px",
  boxSizing: "border-box",
  display: "flex",
  flexDirection: "column",
  gap: "4",
  padding: "{spacing.4} {spacing.3} {spacing.8}",
  md: { padding: "{spacing.8} {spacing.10}" },
});
const headClass = css({ display: "flex", alignItems: "flex-start", gap: "4" });
const headTextClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "1.5" });
const titleClass = css({ display: "none", margin: 0, font: "700 30px/1.2 {fonts.display}", letterSpacing: "-0.01em", md: { display: "block" } });
const captionClass = css({ margin: 0, fontSize: "15px", lineHeight: 1.5, color: "fg.muted" });
const actionsClass = css({ display: "flex", gap: "2", flex: "none", "&:empty": { display: "none" } });
const segClass = cx(seg(), css({ alignSelf: "flex-start", display: "none", md: { display: "inline-flex" } }));
const drawerHeadClass = css({ display: "flex", alignItems: "center", padding: "{spacing.3} {spacing.3} {spacing.2} {spacing.5}", "& b": { flex: 1, font: "700 20px/1.3 {fonts.display}" } });
const drawerNavClass = css({ display: "flex", flexDirection: "column", gap: "1", padding: "0 {spacing.3} {spacing.3}", minHeight: "calc(100dvh - 64px)", boxSizing: "border-box" });
const drawerItemClass = css({
  display: "flex",
  alignItems: "center",
  minHeight: "48px",
  padding: "0 {spacing.3}",
  border: 0,
  borderRadius: "control",
  bg: "transparent",
  color: "fg",
  font: "600 15px/1 {fonts.body}",
  textAlign: "left",
  cursor: "pointer",
  _hover: { bg: "state.hover" },
  _currentPage: { bg: "state.selected" },
});
const drawerBackClass = cx(drawerItemClass, css({ mt: "auto", color: "fg.muted" }));
const stubClass = cx(empty(), css({ pt: "16", "& h2": { margin: 0, font: "700 24px/1.25 {fonts.display}" } }));
</script>

<template>
  <div :class="rootClass">
    <header :class="topBarClass" data-theme="night">
      <button type="button" :class="bigIconClass" aria-label="Open admin navigation" @click="sidebarOpen = true">
        <Menu aria-hidden="true" />
      </button>
      <b>{{ activeLabel }}</b>
    </header>

    <AppDrawer v-model:open="sidebarOpen" side="left" label="Admin navigation drawer" bare>
      <div :class="drawerHeadClass">
        <b>Admin</b>
        <button type="button" :class="bigIconClass" aria-label="Close admin navigation" @click="sidebarOpen = false">
          <X aria-hidden="true" />
        </button>
      </div>
      <nav :class="drawerNavClass" aria-label="Admin sections">
        <button
          v-for="panel in NAV"
          :key="panel.slug"
          type="button"
          :class="drawerItemClass"
          :aria-current="panel.slug === activePanel ? 'page' : undefined"
          @click="onPanelClick(panel)"
        >
          {{ panel.label }}
        </button>
        <button type="button" :class="drawerBackClass" @click="emit('back')">Back to chat</button>
      </nav>
    </AppDrawer>

    <main :class="mainClass">
      <header :class="headClass">
        <div :class="headTextClass">
          <h1 :class="titleClass">Admin</h1>
          <p :class="captionClass">{{ active?.caption }}</p>
        </div>
        <div id="admin-header-actions" :class="actionsClass" />
      </header>
      <nav aria-label="Admin sections" :class="segClass">
        <button
          v-for="panel in NAV"
          :key="panel.slug"
          type="button"
          :aria-current="panel.slug === activePanel ? 'page' : undefined"
          @click="onPanelClick(panel)"
        >
          {{ panel.label }}
        </button>
      </nav>

      <slot v-if="activePanel === 'users'" name="users" />
      <slot v-else-if="activePanel === 'spaces'" name="spaces" />
      <slot v-else-if="activePanel === 'channels'" name="channels" />
      <div v-else-if="isStub" :class="stubClass" role="status">
        <img :src="spotError" alt="" />
        <h2>{{ activeLabel }} is coming</h2>
        <p>This panel isn't built yet.</p>
      </div>
    </main>
  </div>
</template>
