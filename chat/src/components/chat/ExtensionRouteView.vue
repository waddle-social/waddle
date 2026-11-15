<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { ExternalLink, Grid3X3, List, RefreshCw, WifiOff, X } from "lucide-vue-next";
import spot from "@waddle/design/assets/spot-empty.svg?url";
import { css, cx } from "styled-system/css";
import { btn, empty, iconbtn } from "styled-system/recipes";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ExtensionAnnotationAction } from "@/lib/chat-ui";
import type { DiscoveredExtensionRoute, ExtensionRouteItem } from "@/lib/xmpp/extension-commands";

const props = defineProps<{
  waddle: SpaceSummary | null;
  channel: ChannelSummary | null;
  route: DiscoveredExtensionRoute | null;
  requestedRoute?: { pluginId: string; routeId: string } | null;
  roomJid: string | null;
  xmppClient?: BrowserXmppClient | null;
  actionError?: string;
}>();

const emit = defineEmits<{
  openNav: [];
  close: [];
  invokeAction: [action: ExtensionAnnotationAction];
}>();

const items = ref<ExtensionRouteItem[]>([]);
const state = ref<"idle" | "loading" | "error">("idle");
const detail = ref("");
const panelRef = ref<HTMLElement | null>(null);

const routeKey = computed(() =>
  props.route && props.roomJid
    ? `${props.route.serviceJid}:${props.route.pluginId}:${props.route.routeId}:${props.roomJid}`
    : props.requestedRoute && props.roomJid
      ? `missing:${props.requestedRoute.pluginId}:${props.requestedRoute.routeId}:${props.roomJid}`
    : "",
);

const surfaceIcon = computed(() => props.route?.surface === "gallery" ? Grid3X3 : List);

// Derives "No saved links yet" / "No pins yet" / etc. from the route's
// label. Falls back to a generic "Nothing here yet" when no route is
// resolved (loading / error / missing-route states render different
// branches; this is purely for the empty `items` happy path).
const emptyHeadline = computed(() => {
  const label = props.route?.label?.trim();
  if (!label) return "Nothing here yet";
  return `No ${label.toLowerCase()} yet`;
});
let refreshRequestId = 0;

async function refreshItems() {
  const requestId = ++refreshRequestId;
  if (!props.route || !props.roomJid || !props.xmppClient) {
    items.value = [];
    if (props.requestedRoute && props.roomJid) {
      state.value = "error";
      detail.value = "Extension route is not available.";
    } else {
      state.value = "idle";
      detail.value = "";
    }
    return;
  }
  const requestedRouteKey = routeKey.value;
  state.value = "loading";
  detail.value = "";
  try {
    const nextItems = await props.xmppClient.fetchExtensionRouteItems(props.route, props.roomJid);
    if (requestId !== refreshRequestId || requestedRouteKey !== routeKey.value) return;
    items.value = nextItems;
    state.value = "idle";
  } catch (error) {
    if (requestId !== refreshRequestId || requestedRouteKey !== routeKey.value) return;
    items.value = [];
    state.value = "error";
    detail.value = error instanceof Error ? error.message : "Could not load extension route.";
  }
}

function itemTitle(item: ExtensionRouteItem): string {
  return item.title || item.link?.href || item.id || "Untitled";
}

function fieldLabel(field: { name: string; label?: string }): string {
  if (field.label) return field.label;
  return field.name.replace(/[-_:#]+/g, " ").replace(/\b\w/g, (char) => char.toUpperCase());
}

function safeUrl(href: string | undefined): string | null {
  if (!href) return null;
  try {
    const url = new URL(href);
    return url.protocol === "http:" || url.protocol === "https:" ? url.toString() : null;
  } catch {
    return null;
  }
}

function focusPanel() {
  void nextTick(() => panelRef.value?.focus({ preventScroll: true }));
}

watch(routeKey, () => {
  focusPanel();
  void refreshItems();
}, { immediate: true });

onMounted(focusPanel);
// App-Room extension panel: titled rail, floe item cards, the spot empty state.
const cls = {
  root: css({ display: "flex", flexDirection: "column", flex: 1, minWidth: 0, bg: "bg.rail", outline: "none" }),
  head: css({ display: "flex", alignItems: "center", gap: "2", flex: "none", minHeight: "header", px: "4", borderBottom: "1px solid {colors.border.soft}" }),
  nav: css({ lg: { display: "none" } }),
  sigil: css({ width: "18px", height: "18px", flex: "none", color: "fg.soft" }),
  titles: css({ flex: 1, minWidth: 0 }),
  title: css({ margin: 0, font: "700 16px/1.3 {fonts.display}", color: "fg", truncate: true }),
  caption: css({ textStyle: "caption", color: "fg.muted", truncate: true }),
  scroll: css({ flex: 1, minHeight: 0, overflowX: "hidden", overflowY: "auto", p: "3" }),
  loading: css({ display: "flex", height: "100%", alignItems: "center", justifyContent: "center", textStyle: "caption", color: "fg.muted" }),
  error: css({ display: "flex", alignItems: "center", gap: "3", p: "4", borderRadius: "panel", bg: "danger.subtle", color: "danger.text", fontSize: "sm", fontWeight: 600, "& svg": { width: "16px", height: "16px", flex: "none" } }),
  empty: css({ minHeight: "100%", justifyContent: "center" }),
  emptyTitle: css({ margin: 0, font: "700 24px/1.2 {fonts.display}", color: "fg", textWrap: "balance" }),
  list: css({ display: "grid", gap: "2" }),
  gallery: css({ display: "grid", gap: "3", gridTemplateColumns: "1fr", sm: { gridTemplateColumns: "repeat(2, minmax(0, 1fr))" }, xl: { gridTemplateColumns: "repeat(3, minmax(0, 1fr))" } }),
  card: css({ minWidth: 0, display: "flex", flexDirection: "column", gap: "2", p: "4", borderRadius: "panel", bg: "surface", border: "1px solid {colors.border.soft}" }),
  cardHead: css({ display: "flex", minWidth: 0, alignItems: "flex-start", justifyContent: "space-between", gap: "3" }),
  cardTitle: css({ margin: 0, fontSize: "sm", fontWeight: 700, color: "fg", truncate: true }),
  cardBody: css({ margin: 0, textStyle: "caption", color: "fg", whiteSpace: "pre-line", overflowWrap: "anywhere" }),
  fields: css({ display: "grid", gap: "1", m: 0 }),
  field: css({ display: "grid", gridTemplateColumns: "minmax(5.5rem, 0.45fr) 1fr", gap: "2", "& dt, & dd": { m: 0, textStyle: "caption", truncate: true, minWidth: 0 }, "& dt": { color: "fg.muted" }, "& dd": { color: "fg" } }),
  chips: css({ display: "flex", flexWrap: "wrap", gap: "1.5" }),
  option: css({ textStyle: "caption", px: "2", py: "1", borderRadius: "sm", bg: "surface.2", color: "fg.muted" }),
};
</script>

<template>
  <aside
    ref="panelRef"
    :class="cls.root"
    tabindex="-1"
    :aria-label="route?.label ? `${route.label} extension route` : 'Extension route'"
  >
    <header :class="cls.head">
      <button
        :class="cx(iconbtn(), cls.nav)"
        type="button"
        aria-label="Open navigation"
        @click="emit('openNav')"
      >
        <List aria-hidden="true" />
      </button>
      <component :is="surfaceIcon" :class="cls.sigil" aria-hidden="true" />
      <div :class="cls.titles">
        <h2 :class="cls.title">{{ route?.label ?? "Extension" }}</h2>
        <div :class="cls.caption">
          {{ channel?.name ?? "Channel" }}<span v-if="waddle"> · {{ waddle.name }}</span>
        </div>
      </div>
      <button
        :class="iconbtn()"
        type="button"
        aria-label="Refresh"
        :disabled="state === 'loading'"
        @click="refreshItems"
      >
        <RefreshCw aria-hidden="true" />
      </button>
      <button
        :class="iconbtn()"
        type="button"
        aria-label="Close extension route"
        @click="emit('close')"
      >
        <X aria-hidden="true" />
      </button>
    </header>

    <div :class="cls.scroll">
      <div v-if="state === 'loading'" :class="cls.loading" role="status">
        Loading…
      </div>
      <div v-else-if="state === 'error'" :class="cls.error">
        <WifiOff aria-hidden="true" />
        <span>{{ detail }}</span>
      </div>
      <div v-else-if="actionError" :class="cls.error">
        <WifiOff aria-hidden="true" />
        <span>{{ actionError }}</span>
      </div>
      <!-- Headline derives from the route's label ("No saved links yet"),
           so any extension route gets a meaningful empty state. -->
      <div v-else-if="items.length === 0" :class="cx(empty(), cls.empty)" role="status">
        <img :src="spot" alt="" />
        <h3 :class="cls.emptyTitle">{{ emptyHeadline }}</h3>
        <p>Items added through {{ route?.label ?? "this extension" }} will appear here.</p>
      </div>
      <div v-else :class="route?.surface === 'gallery' ? cls.gallery : cls.list">
        <article
          v-for="item in items"
          :key="item.id ?? itemTitle(item)"
          :class="cls.card"
        >
          <div :class="cls.cardHead">
            <div :class="cls.titles">
              <h3 :class="cls.cardTitle">{{ itemTitle(item) }}</h3>
              <p v-if="item.subtitle" :class="cls.caption">{{ item.subtitle }}</p>
            </div>
            <a
              v-if="safeUrl(item.link?.href)"
              :class="iconbtn({ size: 'sm' })"
              :href="safeUrl(item.link?.href) ?? '#'"
              target="_blank"
              rel="noopener noreferrer"
              aria-label="Open link"
            >
              <ExternalLink aria-hidden="true" />
            </a>
          </div>
          <p v-if="item.description" :class="cls.cardBody">{{ item.description }}</p>
          <dl v-if="item.fields.length > 0" :class="cls.fields">
            <div v-for="field in item.fields" :key="field.name" :class="cls.field">
              <dt>{{ fieldLabel(field) }}</dt>
              <dd>{{ field.value }}</dd>
            </div>
          </dl>
          <div v-if="item.options.length > 0" :class="cls.chips">
            <span v-for="option in item.options" :key="option.id" :class="cls.option">
              {{ option.label }}
            </span>
          </div>
          <div v-if="item.actions.length > 0" :class="cls.chips">
            <button
              v-for="action in item.actions"
              :key="action.launchId"
              type="button"
              :class="btn({ size: 'sm' })"
              @click="emit('invokeAction', { label: action.label, route: action.launchId, launch: action.launch })"
            >
              {{ action.label }}
            </button>
          </div>
        </article>
      </div>
    </div>
  </aside>
</template>
