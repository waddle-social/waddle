<script setup lang="ts">
import { computed, ref } from "vue";
import { Menu } from "@ark-ui/vue/menu";
import { AtSign, Bell, BellOff, Check, ChevronRight } from "lucide-vue-next";
import AppMenu from "@/components/ui/AppMenu.vue";
import { menuClasses, menuItemHintClass, menuItemIconClass, menuItemStackClass } from "@/ui/menu-classes";
import type { BrowserXmppClient, NotifyMode } from "@/lib/xmpp-client";
import {
  effectiveNotifyMode,
  NOTIFY_MODE_HINT,
  NOTIFY_MODE_LABEL,
  type ConversationKind,
  type NotifySettingsStore,
} from "@/lib/notify-settings";

/** Per-chat XEP-0492 notification mode picker (#532).
 *
 * Renders as an icon button reflecting the current effective mode for
 * the conversation. Clicking opens an Ark Menu with three radio
 * options. Selecting one publishes a XEP-0402 bookmark item update
 * to PEP via [[BrowserXmppClient.setRoomNotificationMode]].
 *
 * Accessibility:
 * * The menu is a `role="menu"` with three `menuitemradio` children and
 *   one `menuitemcheckbox` (WAI-ARIA APG menu pattern); Ark owns the
 *   keyboard navigation, outside-click dismissal, Escape and focus return.
 * * Picking a mode keeps the menu open with a "Saving…" note until the
 *   publish resolves, then closes; a failure stays open with the error.
 * * The trigger is disabled (`aria-disabled`) while no client is wired
 *   or the store is still hydrating, so clicks don't open a menu
 *   that can't act.
 *
 * `variant="submenu"` renders the trigger as a row inside a parent
 * `AppMenu` (the compact header's overflow menu) instead of an icon
 * button; the picker itself is identical.
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
  variant?: "icon" | "submenu";
}>();

const open = ref(false);
const submitting = ref<NotifyMode | null>(null);
const submittingRich = ref(false);
const errorMessage = ref<string | null>(null);

const MODES: NotifyMode[] = ["always", "on-mention", "never"];
const RICH_VALUE = "rich-previews";

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

const statusLabel = computed(() => {
  if (props.client === null) return "connecting…";
  if (props.store.hydrating.value) return "syncing…";
  return NOTIFY_MODE_LABEL[currentMode.value];
});
const buttonTitle = computed(() => `Notifications: ${statusLabel.value}`);

function onOpenChange(next: boolean) {
  if (disabled.value && next) return;
  open.value = next;
  // Drop the error banner on dismissal so it doesn't reappear the next
  // time the user opens the menu.
  if (!next) errorMessage.value = null;
}

function close() {
  open.value = false;
  errorMessage.value = null;
}

function onSelect(value: string) {
  if (value === RICH_VALUE) {
    void toggleRichPayload();
    return;
  }
  if ((MODES as string[]).includes(value)) {
    void selectMode(value as NotifyMode);
  }
}

async function selectMode(mode: NotifyMode) {
  if (!props.client) return;
  if (busy.value) return;
  if (mode === currentMode.value) {
    close();
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
      // instead of leaving the user with a silent dead menu.
      console.warn("[NotifyModeButton] props.store.setMode threw:", error);
      errorMessage.value = "Couldn't save the setting. Try again in a moment.";
      return;
    }
    if (result === "ok") {
      close();
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
    // which dismisses), so the menu stays open on success.
    if (result === "node-config-mismatch") {
      errorMessage.value = nodeMismatchMessage.value;
    } else if (result !== "ok") {
      errorMessage.value = "Couldn't save the setting. Try again in a moment.";
    }
  } finally {
    submittingRich.value = false;
  }
}
</script>

<template>
  <AppMenu
    v-if="roomJid"
    :open="open"
    :tooltip="buttonTitle"
    :submenu="variant === 'submenu'"
    placement="bottom-end"
    aria-label="Notification mode"
    content-class="w-64 max-w-[calc(100vw-1rem)]"
    @update:open="onOpenChange"
    @select="onSelect"
  >
    <template #trigger>
      <div
        v-if="variant === 'submenu'"
        :class="[menuClasses.item, 'py-2', disabled ? 'cursor-not-allowed opacity-50' : '']"
        style="height: auto"
        :aria-disabled="disabled"
      >
        <span :class="menuItemIconClass" aria-hidden="true"><component :is="icon" class="h-4 w-4" /></span>
        <span :class="[menuItemStackClass, 'flex-1']">
          <span class="type-control text-foreground">Notifications</span>
          <span :class="menuItemHintClass">{{ statusLabel }}</span>
        </span>
        <ChevronRight class="h-3.5 w-3.5 text-muted-foreground" aria-hidden="true" />
      </div>
      <button
        v-else
        class="chat-icon-button chat-icon-button--md"
        :class="disabled
          ? 'opacity-50 cursor-not-allowed text-muted-foreground'
          : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
        type="button"
        :aria-label="buttonTitle"
        :disabled="disabled"
        :aria-disabled="disabled"
      >
        <component :is="icon" class="w-3.5 h-3.5" />
      </button>
    </template>
    <Menu.RadioItemGroup :model-value="currentMode">
      <Menu.ItemGroupLabel :class="menuClasses.itemGroupLabel">Notifications for this chat</Menu.ItemGroupLabel>
      <Menu.RadioItem
        v-for="mode in MODES"
        :key="mode"
        :value="mode"
        :disabled="busy"
        :close-on-select="false"
        :class="[menuClasses.item, 'flex-col !items-start !gap-0.5 py-2 data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50']"
        style="height: auto"
      >
        <div class="flex w-full items-center justify-between gap-2">
          <Menu.ItemText class="type-field text-foreground">{{ NOTIFY_MODE_LABEL[mode] }}</Menu.ItemText>
          <span v-if="submitting === mode" class="type-meta text-muted-foreground">Saving…</span>
          <Menu.ItemIndicator v-else class="type-meta text-primary">Current</Menu.ItemIndicator>
        </div>
        <span class="type-meta text-muted-foreground">{{ NOTIFY_MODE_HINT[mode] }}</span>
      </Menu.RadioItem>
    </Menu.RadioItemGroup>

    <Menu.Separator :class="menuClasses.separator" />
    <Menu.CheckboxItem
      :value="RICH_VALUE"
      :checked="richOptIn"
      :disabled="busy"
      :close-on-select="false"
      :class="[menuClasses.item, 'flex-col !items-start !gap-0.5 py-2 data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50']"
      style="height: auto"
    >
      <div class="flex w-full items-center justify-between gap-2">
        <Menu.ItemText class="type-field text-foreground">Rich notification previews</Menu.ItemText>
        <span v-if="submittingRich" class="type-meta text-muted-foreground">Saving…</span>
        <Menu.ItemIndicator v-else>
          <Check class="h-3.5 w-3.5 text-primary" aria-hidden="true" />
        </Menu.ItemIndicator>
      </div>
      <span class="type-meta text-muted-foreground">Show the sender and a message preview in push notifications for this chat.</span>
    </Menu.CheckboxItem>

    <p
      v-if="errorMessage"
      class="type-meta mt-1 rounded-md bg-destructive/10 px-2 py-1.5 text-destructive-text"
      role="alert"
      aria-live="assertive"
    >{{ errorMessage }}</p>
  </AppMenu>
</template>
