<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { Bell, BellMinus, BellOff } from "lucide-vue-next";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import {
  effectiveNotifyMode,
  notifySettingsStore,
  NOTIFY_MODE_HINT,
  NOTIFY_MODE_LABEL,
  type ConversationKind,
  type NotifyMode,
} from "@/lib/notify-settings";

/** Per-chat XEP-0492 notification mode picker (#532).
 *
 * Renders as an icon button reflecting the current effective mode for
 * the conversation. Clicking opens a small popover with three radio
 * options. Selecting one publishes a XEP-0402 bookmark item update
 * to PEP via [[BrowserXmppClient.setRoomNotificationMode]].
 *
 * The button is hidden when `roomJid` is empty (DMs in this v1 slice
 * have no XEP-0492 carrier — per-DM settings ship in #720). */
const props = defineProps<{
  /** Bare JID of the room whose XEP-0402 bookmark carries the
   * XEP-0492 `<notify/>` setting. Empty string disables the
   * control (caller should `v-if` instead, but the empty-string
   * guard is defence-in-depth for DM panes). */
  roomJid: string;
  /** Conversation kind for XEP-0492 §3 default resolution. The chat
   * layer currently lacks a public/private discriminator on
   * `ChannelSummary`; callers pass `"private-group"` for all
   * channels until that flag lands. */
  conversationKind: ConversationKind;
  /** Initial room display name copied into the bookmark on first
   * publish (XEP-0402 §2.2 `name` attribute). Optional — when
   * omitted the bookmark is created without a name attribute. */
  roomName?: string;
  /** Wired XMPP client used to publish the update. */
  client: BrowserXmppClient | null;
}>();

const store = notifySettingsStore;
const open = ref(false);
const submitting = ref<NotifyMode | null>(null);
const rootEl = ref<HTMLElement | null>(null);

const currentMode = computed<NotifyMode>(() => {
  const bookmark = store.bookmarks.value[props.roomJid];
  return effectiveNotifyMode(bookmark, props.conversationKind);
});

const icon = computed(() => {
  switch (currentMode.value) {
    case "always":
      return Bell;
    case "on-mention":
      return BellMinus;
    case "never":
      return BellOff;
  }
  return Bell;
});

const buttonTitle = computed(() => `Notifications: ${NOTIFY_MODE_LABEL[currentMode.value]}`);

async function selectMode(mode: NotifyMode) {
  if (!props.client) return;
  if (submitting.value !== null) return;
  if (mode === currentMode.value) {
    open.value = false;
    return;
  }
  submitting.value = mode;
  try {
    const ok = await store.setMode(props.client, {
      roomJid: props.roomJid,
      mode,
      name: props.roomName,
    });
    if (ok) open.value = false;
  } finally {
    submitting.value = null;
  }
}

function onWindowClick(event: MouseEvent) {
  if (!open.value) return;
  const target = event.target as Node | null;
  if (target && rootEl.value && !rootEl.value.contains(target)) {
    open.value = false;
  }
}

function onEsc(event: KeyboardEvent) {
  if (event.key === "Escape") open.value = false;
}

onMounted(() => {
  window.addEventListener("mousedown", onWindowClick);
  window.addEventListener("keydown", onEsc);
});

onUnmounted(() => {
  window.removeEventListener("mousedown", onWindowClick);
  window.removeEventListener("keydown", onEsc);
});

const modes: NotifyMode[] = ["always", "on-mention", "never"];
</script>

<template>
  <div v-if="roomJid" ref="rootEl" class="relative inline-flex">
    <button
      class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground"
      type="button"
      :title="buttonTitle"
      :aria-label="buttonTitle"
      :aria-expanded="open"
      :aria-haspopup="true"
      @click="open = !open"
    >
      <component :is="icon" class="w-3.5 h-3.5" />
    </button>
    <div
      v-if="open"
      class="absolute right-0 top-[calc(100%+4px)] z-30 w-64 rounded-lg border border-border bg-popover p-2 shadow-lg"
      role="menu"
      aria-label="Notification mode"
    >
      <p class="type-section-label px-2 pb-1 pt-1 text-muted-foreground">Notifications for this chat</p>
      <button
        v-for="mode in modes"
        :key="mode"
        type="button"
        role="menuitemradio"
        :aria-checked="currentMode === mode"
        :disabled="submitting !== null"
        class="flex w-full flex-col items-start gap-0.5 rounded-md px-2 py-2 text-left hover:bg-muted focus:bg-muted focus:outline-none"
        :class="{ 'bg-muted/60': currentMode === mode }"
        @click="selectMode(mode)"
      >
        <div class="flex w-full items-center justify-between">
          <span class="type-field text-foreground">{{ NOTIFY_MODE_LABEL[mode] }}</span>
          <span v-if="submitting === mode" class="type-meta text-muted-foreground">Saving…</span>
          <span v-else-if="currentMode === mode" class="type-meta text-primary">Current</span>
        </div>
        <span class="type-meta text-muted-foreground">{{ NOTIFY_MODE_HINT[mode] }}</span>
      </button>
    </div>
  </div>
</template>
