<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { AtSign, Bell, BellOff } from "lucide-vue-next";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import {
  effectiveNotifyMode,
  nextMenuIndex,
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
 * Accessibility (round-7 UX P1):
 * * The popover is keyed as a `role="menu"` with three `menuitemradio`
 *   children, matching the WAI-ARIA APG menu radio pattern.
 * * On open, focus moves into the currently-selected option so screen
 *   readers announce the active mode.
 * * Arrow Up/Down move focus between options. Home / End jump to first
 *   / last. Esc closes and returns focus to the trigger.
 * * The trigger is disabled (`aria-disabled`) while no client is wired
 *   or the store is still hydrating, so clicks don't open a popover
 *   that can't act.
 */
const props = defineProps<{
  /** Bare JID of the room whose XEP-0402 bookmark carries the
   * XEP-0492 `<notify/>` setting. */
  roomJid: string;
  /** Conversation kind for XEP-0492 §3 default resolution. */
  conversationKind: ConversationKind;
  /** Initial room display name copied into the bookmark on first
   * publish (XEP-0402 §2.2 `name` attribute). */
  roomName?: string;
  /** Wired XMPP client used to publish the update. */
  client: BrowserXmppClient | null;
}>();

const store = notifySettingsStore;
const open = ref(false);
const submitting = ref<NotifyMode | null>(null);
const errorMessage = ref<string | null>(null);
const rootEl = ref<HTMLElement | null>(null);
const triggerEl = ref<HTMLButtonElement | null>(null);
const optionRefs = ref<(HTMLButtonElement | null)[]>([]);

const MODES: NotifyMode[] = ["always", "on-mention", "never"];

const currentMode = computed<NotifyMode>(() => {
  const bookmark = store.bookmarks.value[props.roomJid];
  return effectiveNotifyMode(bookmark, props.conversationKind);
});

const icon = computed(() => {
  switch (currentMode.value) {
    case "always":
      return Bell;
    case "on-mention":
      return AtSign;
    case "never":
      return BellOff;
  }
});

const disabled = computed(() => props.client === null || store.hydrating.value);

const buttonTitle = computed(() => {
  if (props.client === null) return "Notifications: connecting…";
  if (store.hydrating.value) return "Notifications: syncing…";
  return `Notifications: ${NOTIFY_MODE_LABEL[currentMode.value]}`;
});

async function toggleOpen() {
  if (disabled.value) return;
  open.value = !open.value;
  if (open.value) {
    await nextTick();
    focusOptionAt(MODES.indexOf(currentMode.value));
  }
}

function focusOptionAt(index: number) {
  const refs = optionRefs.value;
  if (refs.length === 0) return;
  const clamped = ((index % refs.length) + refs.length) % refs.length;
  refs[clamped]?.focus();
}

function focusedIndex(): number {
  const active = document.activeElement as HTMLElement | null;
  return optionRefs.value.findIndex((el) => el === active);
}

function closeAndReturnFocus() {
  open.value = false;
  errorMessage.value = null;
  triggerEl.value?.focus();
}

async function selectMode(mode: NotifyMode) {
  if (!props.client) return;
  if (submitting.value !== null) return;
  if (mode === currentMode.value) {
    closeAndReturnFocus();
    return;
  }
  submitting.value = mode;
  errorMessage.value = null;
  try {
    const result = await store.setMode(props.client, {
      roomJid: props.roomJid,
      mode,
      name: props.roomName,
    });
    if (result === "ok") {
      closeAndReturnFocus();
    } else if (result === "node-config-mismatch") {
      // Round-8 UX P2: distinguish the recoverable XEP-0060
      // precondition-not-met case so the user gets an actionable
      // hint instead of a generic "didn't save".
      errorMessage.value =
        "This room's settings node was created by an older client. Ask a server admin to delete the node so Waddle can re-create it.";
    } else {
      errorMessage.value = "Couldn't save the setting. Try again in a moment.";
    }
  } finally {
    submitting.value = null;
  }
}

function onMenuKeydown(event: KeyboardEvent) {
  switch (event.key) {
    case "ArrowDown":
    case "ArrowRight":
      event.preventDefault();
      focusOptionAt(nextMenuIndex(focusedIndex(), 1, MODES.length));
      break;
    case "ArrowUp":
    case "ArrowLeft":
      event.preventDefault();
      focusOptionAt(nextMenuIndex(focusedIndex(), -1, MODES.length));
      break;
    case "Home":
      event.preventDefault();
      focusOptionAt(0);
      break;
    case "End":
      event.preventDefault();
      focusOptionAt(MODES.length - 1);
      break;
    case "Escape":
      event.preventDefault();
      closeAndReturnFocus();
      break;
  }
}

function onWindowClick(event: MouseEvent) {
  if (!open.value) return;
  const target = event.target as Node | null;
  if (target && rootEl.value && !rootEl.value.contains(target)) {
    open.value = false;
    // Drop the error banner on dismissal so it doesn't reappear
    // the next time the user opens the popover. Round-9 P3.
    errorMessage.value = null;
  }
}

function onWindowEsc(event: KeyboardEvent) {
  // Top-level Esc when focus is somewhere else (e.g. user typed
  // in the message composer while the popover was open).
  if (event.key === "Escape" && open.value) closeAndReturnFocus();
}

onMounted(() => {
  window.addEventListener("mousedown", onWindowClick);
  window.addEventListener("keydown", onWindowEsc);
});

onUnmounted(() => {
  window.removeEventListener("mousedown", onWindowClick);
  window.removeEventListener("keydown", onWindowEsc);
});
</script>

<template>
  <div v-if="roomJid" ref="rootEl" class="relative inline-flex">
    <button
      ref="triggerEl"
      class="chat-icon-button chat-icon-button--md"
      :class="disabled
        ? 'opacity-50 cursor-not-allowed text-muted-foreground'
        : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
      type="button"
      :title="buttonTitle"
      :aria-label="buttonTitle"
      :aria-expanded="open"
      :aria-disabled="disabled"
      aria-haspopup="menu"
      @click="toggleOpen"
    >
      <component :is="icon" class="w-3.5 h-3.5" />
    </button>
    <div
      v-if="open"
      class="absolute right-0 top-[calc(100%+4px)] z-30 w-64 max-w-[calc(100vw-1rem)] rounded-lg border border-border bg-popover p-2 shadow-lg"
      role="menu"
      aria-label="Notification mode"
      @keydown="onMenuKeydown"
    >
      <p class="type-section-label px-2 pb-1 pt-1 text-muted-foreground">Notifications for this chat</p>
      <button
        v-for="(mode, index) in MODES"
        :key="mode"
        :ref="(el) => optionRefs[index] = (el as HTMLButtonElement | null)"
        type="button"
        role="menuitemradio"
        :aria-checked="currentMode === mode"
        :disabled="submitting !== null"
        class="flex w-full flex-col items-start gap-0.5 rounded-md px-2 py-2 text-left hover:bg-muted focus:bg-muted focus:outline-none disabled:opacity-50 disabled:cursor-not-allowed"
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
      <p
        v-if="errorMessage"
        class="type-meta mt-1 rounded-md bg-destructive/10 px-2 py-1.5 text-destructive"
        role="alert"
        aria-live="assertive"
      >{{ errorMessage }}</p>
    </div>
  </div>
</template>
