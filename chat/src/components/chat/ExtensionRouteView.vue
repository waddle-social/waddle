<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { ExternalLink, Grid3X3, List, RefreshCw, WifiOff, X } from "lucide-vue-next";
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
</script>

<template>
  <aside
    ref="panelRef"
    class="extension-route-panel flex min-w-0 flex-1 flex-col bg-background border-l border-border"
    tabindex="-1"
    :aria-label="route?.label ? `${route.label} extension route` : 'Extension route'"
  >
    <header class="chat-pane-header border-b border-border px-[var(--chat-content-inline)] py-0 flex flex-shrink-0 items-center justify-between gap-3 glass-panel">
      <div class="chat-message-lane flex min-w-0 items-center gap-2">
        <button
          class="chat-icon-button lg:hidden"
          type="button"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <List class="h-4 w-4" />
        </button>
        <component :is="surfaceIcon" class="h-4 w-4 shrink-0 text-primary" />
        <div class="min-w-0">
          <div class="type-pane-title truncate">{{ route?.label ?? "Extension" }}</div>
          <div class="type-caption truncate text-muted-foreground">
            {{ channel?.name ?? "Channel" }}<span v-if="waddle"> · {{ waddle.name }}</span>
          </div>
        </div>
      </div>
      <div class="flex items-center gap-1.5">
        <button
          class="chat-icon-button"
          type="button"
          aria-label="Refresh"
          :disabled="state === 'loading'"
          @click="refreshItems"
        >
          <RefreshCw class="h-4 w-4" :class="state === 'loading' ? 'animate-spin' : ''" />
        </button>
        <button
          class="chat-icon-button"
          type="button"
          aria-label="Close extension route"
          @click="emit('close')"
        >
          <X class="h-4 w-4" />
        </button>
      </div>
    </header>

    <div class="chat-pane-scroll flex-1 px-[var(--chat-content-inline)] py-4">
      <div v-if="state === 'loading'" class="type-caption flex h-full items-center justify-center text-muted-foreground">
        Loading…
      </div>
      <div v-else-if="state === 'error'" class="chat-message-lane flex items-center gap-3 rounded-md border border-destructive/20 bg-destructive/5 p-4 text-destructive">
        <WifiOff class="h-4 w-4 shrink-0" />
        <span class="type-control">{{ detail }}</span>
      </div>
      <div v-else-if="actionError" class="chat-message-lane flex items-center gap-3 rounded-md border border-destructive/20 bg-destructive/5 p-4 text-destructive">
        <WifiOff class="h-4 w-4 shrink-0" />
        <span class="type-control">{{ actionError }}</span>
      </div>
      <div v-else-if="items.length === 0" class="type-caption flex h-full items-center justify-center text-muted-foreground">
        Nothing saved yet.
      </div>
      <div
        v-else
        :class="route?.surface === 'gallery' ? 'grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3' : 'grid gap-2'"
      >
        <article
          v-for="item in items"
          :key="item.id ?? itemTitle(item)"
          class="rounded-md border border-border bg-card p-3 shadow-sm"
        >
          <div class="flex min-w-0 items-start justify-between gap-3">
            <div class="min-w-0">
              <h2 class="type-control type-strong truncate text-card-foreground">{{ itemTitle(item) }}</h2>
              <p v-if="item.subtitle" class="type-caption truncate text-muted-foreground">
                {{ item.subtitle }}
              </p>
            </div>
            <a
              v-if="safeUrl(item.link?.href)"
              class="chat-icon-button chat-icon-button--sm shrink-0"
              :href="safeUrl(item.link?.href) ?? '#'"
              target="_blank"
              rel="noopener noreferrer"
              aria-label="Open link"
            >
              <ExternalLink class="h-3.5 w-3.5" />
            </a>
          </div>
          <p v-if="item.description" class="type-caption mt-2 whitespace-pre-line text-card-foreground">
            {{ item.description }}
          </p>
          <dl v-if="item.fields.length > 0" class="mt-3 grid gap-1">
            <div
              v-for="field in item.fields"
              :key="field.name"
              class="grid grid-cols-[minmax(5.5rem,0.45fr)_1fr] gap-2"
            >
              <dt class="type-caption truncate text-muted-foreground">{{ fieldLabel(field) }}</dt>
              <dd class="type-caption min-w-0 truncate text-card-foreground">{{ field.value }}</dd>
            </div>
          </dl>
          <div v-if="item.options.length > 0" class="mt-3 flex flex-wrap gap-1.5">
            <span
              v-for="option in item.options"
              :key="option.id"
              class="type-caption rounded-md border border-border bg-muted px-2 py-1 text-muted-foreground"
            >
              {{ option.label }}
            </span>
          </div>
          <div v-if="item.actions.length > 0" class="mt-3 flex flex-wrap gap-1.5">
            <button
              v-for="action in item.actions"
              :key="action.launchId"
              type="button"
              class="type-caption inline-flex min-h-8 items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1.5 text-foreground transition-colors hover:bg-muted"
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
