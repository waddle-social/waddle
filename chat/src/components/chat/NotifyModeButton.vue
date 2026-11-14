<script setup lang="ts">
import { computed, ref } from "vue";
import { Menu } from "@ark-ui/vue/menu";
import { AtSign, Bell, BellOff, Check, ChevronRight } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { iconbtn, waddle } from "styled-system/recipes";
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
const MODE_ICON = { always: Bell, "on-mention": AtSign, never: BellOff } as const;
const RICH_VALUE = "rich-previews";

const currentMode = computed<NotifyMode>(() => {
  const bookmark = props.store.bookmarks.value[props.roomJid];
  return effectiveNotifyMode(bookmark, props.conversationKind);
});

const richOptIn = computed<boolean>(() => props.store.getRichPayloadOptIn(props.roomJid));

// Any in-flight publish (mode or opt-in) freezes the whole menu so two
// fetch-merge-publish round-trips can't race on the same bookmark.
const busy = computed(() => submitting.value !== null || submittingRich.value);

const icon = computed(() => MODE_ICON[currentMode.value]);

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

// Notification-mode menu (Rooms-Notify board): icon + label/hint rows,
// the chosen mode on an action tint, a check for the preview opt-in.
const contentClass = css({ width: "min(380px, calc(100vw - 16px))", maxWidth: "none" });
const trigger = cx(iconbtn(), css({ '&[aria-expanded="true"]': { bg: "action.subtle", color: "action.text" } }));
const submenuTrigger = css({ height: "auto", py: "2" });
const submenuDisabled = css({ cursor: "not-allowed", opacity: 0.5 });
const flex1 = css({ flex: 1 });
const labelText = css({ font: "600 14px/1.3 {fonts.body}", color: "fg" });
const option = cx(menuClasses.item, css({
  alignItems: "flex-start",
  height: "auto",
  py: "2.5",
  "& > svg": { mt: "1px" },
}));
const radioOption = cx(option, css({ "&[data-state=checked]": { bg: "action.subtle", "& > svg": { color: "action.text" } } }));
const trailing = css({ display: "inline-flex", alignItems: "center", ml: "auto", flex: "none", alignSelf: "center" });
const saving = css({ transform: "scale(0.7)", transformOrigin: "right center" });
const checkBox = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  width: "18px",
  height: "18px",
  flex: "none",
  mt: "1px",
  borderRadius: "4px",
  border: "1px solid {colors.border.strong}",
  "[data-state=checked] > &": { bg: "action", borderColor: "action", color: "action.fg" },
  "& svg": { width: "12px !important", height: "12px !important", color: "inherit !important", strokeWidth: 3 },
});
const errorClass = css({
  mx: "1",
  mt: "1.5",
  mb: "0.5",
  px: "3",
  py: "2.5",
  borderRadius: "10px",
  border: "1px solid {colors.danger}",
  bg: "danger.subtle",
  color: "danger.text",
  font: "400 13px/1.45 {fonts.body}",
});
</script>

<template>
  <AppMenu
    v-if="roomJid"
    :open="open"
    :tooltip="buttonTitle"
    :submenu="variant === 'submenu'"
    placement="bottom-end"
    aria-label="Notification mode"
    :content-class="contentClass"
    @update:open="onOpenChange"
    @select="onSelect"
  >
    <template #trigger>
      <div
        v-if="variant === 'submenu'"
        :class="[menuClasses.item, submenuTrigger, disabled && submenuDisabled]"
        :aria-disabled="disabled"
      >
        <span :class="menuItemIconClass" aria-hidden="true"><component :is="icon" /></span>
        <span :class="[menuItemStackClass, flex1]">
          <span :class="labelText">Notifications</span>
          <span :class="menuItemHintClass">{{ statusLabel }}</span>
        </span>
        <ChevronRight aria-hidden="true" />
      </div>
      <button
        v-else
        :class="trigger"
        type="button"
        :aria-label="buttonTitle"
        :disabled="disabled"
        :aria-disabled="disabled"
      >
        <component :is="icon" aria-hidden="true" />
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
        :class="radioOption"
      >
        <component :is="MODE_ICON[mode]" aria-hidden="true" />
        <span :class="menuItemStackClass">
          <Menu.ItemText :class="labelText">{{ NOTIFY_MODE_LABEL[mode] }}</Menu.ItemText>
          <span :class="menuItemHintClass">{{ NOTIFY_MODE_HINT[mode] }}</span>
        </span>
        <span v-if="submitting === mode" :class="trailing" role="status" aria-label="Saving…">
          <span :class="[waddle(), saving]" aria-hidden="true"><i /><i /><i /></span>
        </span>
        <Menu.ItemIndicator v-else :class="[trailing, menuItemHintClass]">Current</Menu.ItemIndicator>
      </Menu.RadioItem>
    </Menu.RadioItemGroup>

    <Menu.Separator :class="menuClasses.separator" />
    <Menu.CheckboxItem
      :value="RICH_VALUE"
      :checked="richOptIn"
      :disabled="busy"
      :close-on-select="false"
      :class="option"
    >
      <span :class="checkBox" aria-hidden="true">
        <Menu.ItemIndicator><Check /></Menu.ItemIndicator>
      </span>
      <span :class="menuItemStackClass">
        <Menu.ItemText :class="labelText">Rich notification previews</Menu.ItemText>
        <span :class="menuItemHintClass">Show the sender and a message preview in push notifications for this chat.</span>
      </span>
      <span v-if="submittingRich" :class="trailing" role="status" aria-label="Saving…">
        <span :class="[waddle(), saving]" aria-hidden="true"><i /><i /><i /></span>
      </span>
    </Menu.CheckboxItem>

    <p
      v-if="errorMessage"
      :class="errorClass"
      role="alert"
      aria-live="assertive"
    >{{ errorMessage }}</p>
  </AppMenu>
</template>
