<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { AtSign, Bell, BellOff, Check } from "lucide-vue-next";
import type { BrowserXmppClient, NotifyMode } from "@/lib/xmpp-client";
import {
  effectiveNotifyMode,
  nextMenuIndex,
  NOTIFY_MODE_HINT,
  NOTIFY_MODE_LABEL,
  type ConversationKind,
  type NotifySettingsStore,
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
  /** Per-controller XEP-0492 store. Threaded as a prop instead of a
   * module-level singleton so unrelated test fixtures and (future)
   * multi-account UIs can hold independent state. */
  store: NotifySettingsStore;
}>();

const open = ref(false);
const submitting = ref<NotifyMode | null>(null);
const submittingRich = ref(false);
const errorMessage = ref<string | null>(null);
const rootEl = ref<HTMLElement | null>(null);
const triggerEl = ref<HTMLButtonElement | null>(null);
const optionRefs = ref<(HTMLButtonElement | null)[]>([]);

const MODES: NotifyMode[] = ["always", "on-mention", "never"];
// Menu rows participating in arrow-key navigation: the three mode
// radios plus the rich-payload checkbox at the end.
const MENU_ITEM_COUNT = MODES.length + 1;
const RICH_INDEX = MODES.length;

const currentMode = computed<NotifyMode>(() => {
  const bookmark = props.store.bookmarks.value[props.roomJid];
  return effectiveNotifyMode(bookmark, props.conversationKind);
});

const richOptIn = computed<boolean>(() => props.store.getRichPayloadOptIn(props.roomJid));

// Any in-flight publish (mode or opt-in) freezes the whole menu so two
// fetch-merge-publish round-trips can't race on the same bookmark.
const busy = computed(() => submitting.value !== null || submittingRich.value);

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

const disabled = computed(() => props.client === null || props.store.hydrating.value);

// Round-15 UX: the `node-config-mismatch` recovery copy depends on the
// carrier. Group kinds ride a shared XEP-0402 bookmark node that a
// server admin can delete; a direct chat rides the user's OWN
// `urn:waddle:dm-bookmarks:0` personal PEP node — there is no "room"
// and no "server admin" remediation, so the DM copy points at a
// self-service reset instead.
const nodeMismatchMessage = computed(() =>
  props.conversationKind === "direct-chat"
    ? "Your direct-message notification settings couldn't be saved — their storage was created by an incompatible older client."
    : "This room's settings node was created by an older client. Ask a server admin to delete the node so Waddle can re-create it.",
);

const buttonTitle = computed(() => {
  if (props.client === null) return "Notifications: connecting…";
  if (props.store.hydrating.value) return "Notifications: syncing…";
  return `Notifications: ${NOTIFY_MODE_LABEL[currentMode.value]}`;
});

async function toggleOpen() {
  if (disabled.value) return;
  if (open.value) {
    // Route close through closeAndReturnFocus so the error banner
    // doesn't leak across opens — round-12 reviewer P1.
    closeAndReturnFocus();
    return;
  }
  open.value = true;
  await nextTick();
  focusOptionAt(MODES.indexOf(currentMode.value));
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
  if (busy.value) return;
  if (mode === currentMode.value) {
    closeAndReturnFocus();
    return;
  }
  submitting.value = mode;
  errorMessage.value = null;
  try {
    let result: Awaited<ReturnType<typeof props.store.setMode>>;
    try {
      result = await props.store.setMode(props.client, {
        roomJid: props.roomJid,
        mode,
        kind: props.conversationKind,
        name: props.roomName,
      });
    } catch (error) {
      // Defence-in-depth — `props.store.setMode` is supposed to always
      // resolve with a typed result, but if a lower layer ever
      // regresses to throwing, surface the error in the banner
      // instead of leaving the user with a silent dead popover.
      // Round-13 PR review.
      console.warn("[NotifyModeButton] props.store.setMode threw:", error);
      errorMessage.value = "Couldn't save the setting. Try again in a moment.";
      return;
    }
    if (result === "ok") {
      closeAndReturnFocus();
    } else if (result === "node-config-mismatch") {
      // Round-8 UX P2: distinguish the recoverable XEP-0060
      // precondition-not-met case so the user gets an actionable
      // hint instead of a generic "didn't save". Copy is
      // carrier-aware — see `nodeMismatchMessage`.
      errorMessage.value = nodeMismatchMessage.value;
    } else {
      errorMessage.value = "Couldn't save the setting. Try again in a moment.";
    }
  } finally {
    submitting.value = null;
  }
}

async function toggleRichPayload() {
  if (!props.client) return;
  if (busy.value) return;
  submittingRich.value = true;
  errorMessage.value = null;
  try {
    let result: Awaited<ReturnType<typeof props.store.setRichPayloadOptIn>>;
    try {
      result = await props.store.setRichPayloadOptIn(props.client, {
        roomJid: props.roomJid,
        optIn: !richOptIn.value,
        kind: props.conversationKind,
        name: props.roomName,
      });
    } catch (error) {
      // Defence-in-depth, mirroring selectMode — a lower-layer
      // regression that throws surfaces in the banner rather than
      // leaving the toggle in a silent dead state.
      console.warn("[NotifyModeButton] props.store.setRichPayloadOptIn threw:", error);
      errorMessage.value = "Couldn't save the setting. Try again in a moment.";
      return;
    }
    // The toggle is a stay-put interaction (unlike picking a mode,
    // which dismisses), so the popover stays open on success.
    if (result === "node-config-mismatch") {
      errorMessage.value = nodeMismatchMessage.value;
    } else if (result !== "ok") {
      errorMessage.value = "Couldn't save the setting. Try again in a moment.";
    }
  } finally {
    submittingRich.value = false;
    // The toggle was `disabled` while busy, which blurs it. Now that
    // it has re-enabled, re-land focus on it (after the DOM updates)
    // so keyboard / AT users keep their place in the open menu.
    if (open.value) {
      void nextTick().then(() => focusOptionAt(RICH_INDEX));
    }
  }
}

function onMenuKeydown(event: KeyboardEvent) {
  switch (event.key) {
    case "ArrowDown":
    case "ArrowRight":
      event.preventDefault();
      focusOptionAt(nextMenuIndex(focusedIndex(), 1, MENU_ITEM_COUNT));
      break;
    case "ArrowUp":
    case "ArrowLeft":
      event.preventDefault();
      focusOptionAt(nextMenuIndex(focusedIndex(), -1, MENU_ITEM_COUNT));
      break;
    case "Home":
      event.preventDefault();
      focusOptionAt(0);
      break;
    case "End":
      event.preventDefault();
      focusOptionAt(MENU_ITEM_COUNT - 1);
      break;
    case "Escape":
      event.preventDefault();
      closeAndReturnFocus();
      break;
    case "Tab":
      // WAI-ARIA APG menu pattern: Tab closes the menu and lets
      // browser focus continue out — round-12 reviewer P2. We do
      // NOT preventDefault so the next focusable element receives
      // focus naturally.
      open.value = false;
      errorMessage.value = null;
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
      :disabled="disabled"
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
        :disabled="busy"
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

      <div class="my-1 border-t border-border" role="separator" />
      <button
        :ref="(el) => optionRefs[RICH_INDEX] = (el as HTMLButtonElement | null)"
        type="button"
        role="menuitemcheckbox"
        :aria-checked="richOptIn"
        :disabled="busy"
        class="flex w-full flex-col items-start gap-0.5 rounded-md px-2 py-2 text-left hover:bg-muted focus:bg-muted focus:outline-none disabled:opacity-50 disabled:cursor-not-allowed"
        :class="{ 'bg-muted/60': richOptIn }"
        @click="toggleRichPayload"
      >
        <div class="flex w-full items-center justify-between">
          <span class="type-field text-foreground">Rich notification previews</span>
          <span v-if="submittingRich" class="type-meta text-muted-foreground">Saving…</span>
          <Check v-else-if="richOptIn" class="h-3.5 w-3.5 text-primary" aria-hidden="true" />
        </div>
        <span class="type-meta text-muted-foreground">Show the sender and a message preview in push notifications for this chat.</span>
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
