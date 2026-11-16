<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { css, cx } from "styled-system/css";
import { btn, huddleCtl } from "styled-system/recipes";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { menuClasses } from "@/ui/menu-classes";
import {
  Check,
  Fullscreen,
  LayoutGrid,
  Maximize2,
  MessageSquare,
  Hand,
  Mic,
  MicOff,
  Minimize,
  Minimize2,
  MonitorUp,
  MoreHorizontal,
  PhoneOff,
  PictureInPicture2,
  ScanFace,
  SmilePlus,
  Settings,
  SquareUser,
  Users,
  Video,
  VideoOff,
} from "lucide-vue-next";
import type { CallViewMode } from "@/lib/calls/view-mode";

/**
 * Shared control bar for the in-call surfaces (split + expanded).
 * Holds every call action; the only local state is the open/closed flag
 * of the More ▸ overflow menu — call state stays in the parent's
 * `call-controls` store (mic/cam atoms + toggle functions).
 *
 * Status read-outs (connection quality, the live timer) live in the
 * stage-header now, not here, so this bar is actions-only. Less-used
 * actions (Settings, and future devices/diagnostics entries) live under
 * the More ▸ overflow to keep the bar uncluttered.
 */
const props = defineProps<{
  micEnabled: boolean;
  camEnabled: boolean;
  screenShareEnabled: boolean;
  screenShareSupported: boolean;
  /** True when the parent surface is the expanded variant — flips
   *  the toggle button between "expand" and "collapse". */
  isExpanded: boolean;
  /** True when the parent surface is fullscreen (the immersive takeover). */
  isImmersive?: boolean;
  /** True while the Participants dock is open — drives the participants
   *  button's pressed/expanded state. The parent owns the dock store. */
  participantsOpen: boolean;
  /** Attendee count shown as a badge on the participants button. */
  participantCount: number;
  /** True while the dock is open on the Chat tab — drives the chat
   *  button's pressed/expanded state. The parent owns the dock store. */
  chatOpen: boolean;
  /** Unread call-chat messages shown as a badge on the chat button. */
  chatUnread: number;
  /** The chosen stage layout — drives the view switcher's pressed state. */
  viewMode: CallViewMode;
  /** True while the local self-view tile is hidden from this client's stage. */
  selfViewHidden: boolean;
  /** True when this browser can enter either Document PiP or video PiP. */
  pictureInPictureSupported?: boolean;
  /** True while the call is detached into Picture-in-Picture. */
  pictureInPictureActive?: boolean;
  /** True when raise-hand is offered — group (MUC) calls only (#1029). */
  raisedHandAvailable?: boolean;
  /** True while this client currently has its hand raised. */
  raisedHandActive?: boolean;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCam: [];
  toggleScreenShare: [];
  toggleExpanded: [];
  toggleFullscreen: [];
  toggleParticipants: [];
  toggleChat: [];
  sendReaction: [emoji: string];
  toggleRaisedHand: [];
  openSettings: [];
  setViewMode: [mode: CallViewMode];
  toggleSelfView: [];
  togglePictureInPicture: [];
  hangup: [];
}>();

// The button's own `aria-label` overrides the accessible-name calculation, so a
// nested badge (even with its own label) is never announced. Fold the unread
// count into the label and mark the visual badge `aria-hidden`.
const chatButtonLabel = computed(() => {
  const base = props.chatOpen ? "Close chat" : "Open chat";
  if (props.chatUnread <= 0) return base;
  const plural = props.chatUnread === 1 ? "" : "s";
  return `${base}, ${props.chatUnread} unread message${plural}`;
});

/**
 * Arrow-key navigation for the two-option Gallery/Speaker radiogroup, per the
 * WAI-ARIA radio-group pattern: an arrow moves selection to the other option
 * (wrapping at both ends) and focus follows the selection (roving tabindex).
 * With two options every arrow flips the mode, so no redundant emit fires.
 */
function onViewKeydown(event: KeyboardEvent): void {
  const isArrow =
    event.key === "ArrowRight" ||
    event.key === "ArrowDown" ||
    event.key === "ArrowLeft" ||
    event.key === "ArrowUp";
  if (!isArrow) return;
  event.preventDefault();
  const next: CallViewMode = props.viewMode === "gallery" ? "speaker" : "gallery";
  emit("setViewMode", next);
  const current = event.currentTarget as HTMLElement;
  const other = current.nextElementSibling ?? current.previousElementSibling;
  if (other instanceof HTMLElement) other.focus();
}

/**
 * More ▸ overflow menu (WAI-ARIA menu-button pattern). The trigger
 * carries `aria-haspopup="menu"` + `aria-expanded`; the menu is a
 * `role="menu"` of `menuitem` buttons. Opening focuses the first item;
 * Escape, an outside click, or selecting an item closes it. Selecting an
 * item routes focus to whatever it opens (e.g. the settings dialog),
 * while Escape returns focus to the trigger.
 */
const moreOpen = ref(false);
const reactionsOpen = ref(false);
const reactionsWrapEl = ref<HTMLElement | null>(null);
const reactionsTriggerEl = ref<HTMLButtonElement | null>(null);
const reactionsMenuEl = ref<HTMLElement | null>(null);
const moreWrapEl = ref<HTMLElement | null>(null);
const moreTriggerEl = ref<HTMLButtonElement | null>(null);
const moreMenuEl = ref<HTMLElement | null>(null);
const reactionEmojis = ["👍", "❤️", "😂", "🔥", "👏", "🎉"] as const;

function menuItems(): HTMLElement[] {
  const root = moreMenuEl.value;
  if (!root) return [];
  // Includes the checkbox item (Self-view) alongside the plain action items so
  // every entry stays in the arrow-key roving-focus order.
  return Array.from(
    root.querySelectorAll<HTMLElement>('[role="menuitem"],[role="menuitemcheckbox"]'),
  );
}

function reactionItems(): HTMLElement[] {
  const root = reactionsMenuEl.value;
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>('[role="menuitem"]'));
}

async function openReactions(focusFirst: boolean): Promise<void> {
  reactionsOpen.value = true;
  if (!focusFirst) return;
  await nextTick();
  reactionItems()[0]?.focus();
}

function closeReactions(returnFocus: boolean): void {
  if (!reactionsOpen.value) return;
  reactionsOpen.value = false;
  if (returnFocus) reactionsTriggerEl.value?.focus();
}

function onReactionsTriggerKeydown(event: KeyboardEvent): void {
  if (event.key === "ArrowDown" || event.key === "ArrowRight" || event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    void openReactions(true);
  } else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
    event.preventDefault();
    void openReactions(true).then(() => reactionItems().at(-1)?.focus());
  } else if (event.key === "Escape") {
    closeReactions(true);
  }
}

function onReactionsMenuKeydown(event: KeyboardEvent): void {
  const items = reactionItems();
  if (items.length === 0) return;
  const index = items.indexOf(document.activeElement as HTMLElement);
  switch (event.key) {
    case "Escape":
      event.preventDefault();
      closeReactions(true);
      break;
    case "Tab":
      closeReactions(false);
      break;
    case "ArrowDown":
    case "ArrowRight":
      event.preventDefault();
      items[(index + 1) % items.length]?.focus();
      break;
    case "ArrowUp":
    case "ArrowLeft":
      event.preventDefault();
      items[(index - 1 + items.length) % items.length]?.focus();
      break;
    case "Home":
      event.preventDefault();
      items[0]?.focus();
      break;
    case "End":
      event.preventDefault();
      items.at(-1)?.focus();
      break;
  }
}

async function openMore(focusFirst: boolean): Promise<void> {
  moreOpen.value = true;
  if (!focusFirst) return;
  await nextTick();
  menuItems()[0]?.focus();
}

function closeMore(returnFocus: boolean): void {
  if (!moreOpen.value) return;
  moreOpen.value = false;
  if (returnFocus) moreTriggerEl.value?.focus();
}

function onMoreTriggerKeydown(event: KeyboardEvent): void {
  if (event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    void openMore(true);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    void openMore(true).then(() => menuItems().at(-1)?.focus());
  } else if (event.key === "Escape") {
    closeMore(true);
  }
}

function onMoreMenuKeydown(event: KeyboardEvent): void {
  const items = menuItems();
  if (items.length === 0) return;
  const index = items.indexOf(document.activeElement as HTMLElement);
  switch (event.key) {
    case "Escape":
      event.preventDefault();
      closeMore(true);
      break;
    case "Tab":
      // Leaving the menu by Tab dismisses it without stealing focus.
      closeMore(false);
      break;
    case "ArrowDown":
      event.preventDefault();
      items[(index + 1) % items.length]?.focus();
      break;
    case "ArrowUp":
      event.preventDefault();
      items[(index - 1 + items.length) % items.length]?.focus();
      break;
    case "Home":
      event.preventDefault();
      items[0]?.focus();
      break;
    case "End":
      event.preventDefault();
      items.at(-1)?.focus();
      break;
  }
}

function selectSettings(): void {
  emit("openSettings");
  // Focus moves into the dialog that opens — don't yank it back to the trigger.
  closeMore(false);
}

function selectSelfView(): void {
  emit("toggleSelfView");
  // Toggling self-view leaves no new surface to receive focus, so return it to
  // the trigger as the menu closes.
  closeMore(true);
}

function selectReaction(emoji: string): void {
  emit("sendReaction", emoji);
  closeReactions(true);
}

function onDocumentPointerDown(event: PointerEvent): void {
  const target = event.target as Node | null;
  if (target && reactionsWrapEl.value?.contains(target)) return;
  if (target && moreWrapEl.value?.contains(target)) return;
  closeReactions(false);
  closeMore(false);
}

watch([moreOpen, reactionsOpen], ([moreOpenValue, reactionsOpenValue]) => {
  if (typeof document === "undefined") return;
  if (moreOpenValue || reactionsOpenValue) {
    document.addEventListener("pointerdown", onDocumentPointerDown, true);
  } else {
    document.removeEventListener("pointerdown", onDocumentPointerDown, true);
  }
});

// HuddleStage control bar: round `huddleCtl` buttons (labelled in Expanded,
// icon-only in Split), a Gallery/Speaker segment and a danger hang-up pill.
// On phones every control is a 52px disc (Mobile-Huddle).
const rootClass = css({
  containerType: "inline-size",
  width: "100%",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  gap: "2",
  flexWrap: "wrap",
  _narrow: { gap: "2.5" },
});
const ctlClass = cx(huddleCtl(), css({
  position: "relative",
  "& svg": { width: "20px", height: "20px", flex: "none" },
  _expanded: { bg: "action.subtle", color: "action.text" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
  _narrow: { width: "52px", height: "52px", px: 0, "& svg": { width: "22px", height: "22px" } },
}));
// Labels drop to screen-reader text when the bar runs out of room.
const ctlLabelClass = css({ _narrow: { srOnly: true }, "@container (max-width: 900px)": { srOnly: true } });
const hangupLabelClass = css({ _narrow: { srOnly: true } });
const srOnly = css({ srOnly: true });
const segClass = css({ display: "inline-flex", padding: "3px", gap: "2px", borderRadius: "pill", bg: "surface.2" });
const segItemClass = css({
  display: "inline-flex",
  width: "38px",
  height: "38px",
  alignItems: "center",
  justifyContent: "center",
  border: 0,
  borderRadius: "pill",
  bg: "transparent",
  color: "fg.muted",
  cursor: "pointer",
  "& svg": { width: "18px", height: "18px" },
  _hover: { color: "fg" },
  _checked: { bg: "surface", color: "fg", boxShadow: "0 0 0 1px {colors.border}" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
  _narrow: { width: "46px", height: "46px" },
});
// Dividers mark nothing once the bar wraps (400px huddle column, phones).
const dividerClass = css({ width: "1px", height: "24px", bg: "border.soft", mx: "1", _narrow: { display: "none" }, "@container (max-width: 480px)": { display: "none" } });
const countClass = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  minWidth: "18px",
  height: "18px",
  px: "1",
  borderRadius: "pill",
  bg: "color-mix(in srgb, {colors.fg} 14%, transparent)",
  font: "700 11px/1 {fonts.body}",
  fontVariantNumeric: "tabular-nums",
  _narrow: { position: "absolute", top: "-2px", right: "-2px" },
});
const unreadClass = cx(countClass, css({ bg: "gold", color: "gold.fg" }));
const hangupClass = cx(btn({ variant: "danger" }), css({
  height: "44px",
  minHeight: "44px",
  borderRadius: "pill",
  px: "4.5",
  _narrow: { width: "68px", height: "52px", px: 0, justifyContent: "center" },
}));
const popWrapClass = css({ position: "relative", display: "inline-flex" });
const reactionMenuClass = cx(menuClasses.content, css({
  position: "absolute",
  bottom: "calc(100% + 8px)",
  left: "50%",
  minWidth: 0,
  display: "grid",
  gridTemplateColumns: "repeat(6, 40px)",
  gap: "0.5",
  width: "max-content",
  transform: "translateX(-50%)",
  zIndex: 10,
}));
const reactionItemClass = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  width: "40px",
  height: "40px",
  border: 0,
  borderRadius: "10px",
  bg: "transparent",
  fontSize: "22px",
  lineHeight: 1,
  cursor: "pointer",
  _hover: { bg: "state.hover" },
  _focusVisible: { bg: "state.hover", outline: "2px solid {colors.focus}" },
});
const moreMenuClass = cx(menuClasses.content, css({ position: "absolute", bottom: "calc(100% + 8px)", right: 0, zIndex: 10 }));
const moreItemClass = cx(menuClasses.item, css({
  _hover: { bg: "state.hover" },
  _focusVisible: { bg: "state.hover" },
  "& svg[data-check]": { marginLeft: "auto", color: "action.text" },
}));

onBeforeUnmount(() => {
  if (typeof document !== "undefined") {
    document.removeEventListener("pointerdown", onDocumentPointerDown, true);
  }
});
</script>

<template>
  <div :class="rootClass">
    <AppTooltip :label="micEnabled ? 'Mute' : 'Unmute'">
      <button
        type="button"
        :class="ctlClass"
        :aria-pressed="!micEnabled"
        :data-off="!micEnabled"
        @click="emit('toggleMic')"
      >
        <component :is="micEnabled ? Mic : MicOff" aria-hidden="true" />
        <span :class="isExpanded ? ctlLabelClass : srOnly">{{ micEnabled ? "Mute" : "Unmute" }}</span>
      </button>
    </AppTooltip>
    <AppTooltip :label="camEnabled ? 'Camera off' : 'Camera on'">
      <button
        type="button"
        :class="ctlClass"
        :aria-pressed="!camEnabled"
        @click="emit('toggleCam')"
      >
        <component :is="camEnabled ? Video : VideoOff" aria-hidden="true" />
        <span :class="isExpanded ? ctlLabelClass : srOnly">{{ camEnabled ? "Off" : "On" }}</span>
      </button>
    </AppTooltip>
    <AppTooltip v-if="screenShareSupported" :label="screenShareEnabled ? 'Stop sharing' : 'Share screen'">
      <button
        type="button"
        :class="ctlClass"
        :aria-pressed="screenShareEnabled"
        @click="emit('toggleScreenShare')"
      >
        <MonitorUp aria-hidden="true" />
        <span :class="isExpanded ? ctlLabelClass : srOnly">{{ screenShareEnabled ? "Stop" : "Share" }}</span>
      </button>
    </AppTooltip>

    <!-- Gallery ⟷ Speaker view switcher. Sticky for the call; a pin or an
         incoming screen share still overrides the chosen layout's large tile.
         A radiogroup (single-choice) rather than two independent toggles. -->
    <div :class="segClass" role="radiogroup" aria-label="Stage view">
      <AppTooltip label="Gallery view">
        <button
          type="button"
          role="radio"
          :class="segItemClass"
          aria-label="Gallery view"
          :aria-checked="viewMode === 'gallery'"
          :tabindex="viewMode === 'gallery' ? 0 : -1"
          @click="emit('setViewMode', 'gallery')"
          @keydown="onViewKeydown"
        >
          <LayoutGrid aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip label="Speaker view">
        <button
          type="button"
          role="radio"
          :class="segItemClass"
          aria-label="Speaker view"
          :aria-checked="viewMode === 'speaker'"
          :tabindex="viewMode === 'speaker' ? 0 : -1"
          @click="emit('setViewMode', 'speaker')"
          @keydown="onViewKeydown"
        >
          <SquareUser aria-hidden="true" />
        </button>
      </AppTooltip>
    </div>

    <span :class="dividerClass" aria-hidden="true" />

    <!-- Three sizes, one step at a time: split ⇄ expanded ⇄ fullscreen. Each
         size offers at most one step down and one step up, and fullscreen
         offers only its exit, so the buttons never stack into another
         "bigger". -->
    <AppTooltip v-if="!isImmersive" :label="isExpanded ? 'Collapse call' : 'Expand call'">
      <button
        type="button"
        :class="ctlClass"
        :aria-label="isExpanded ? 'Collapse call to split view' : 'Expand call to fill the chat pane'"
        @click="emit('toggleExpanded')"
      >
        <component :is="isExpanded ? Minimize2 : Maximize2" aria-hidden="true" />
      </button>
    </AppTooltip>
    <AppTooltip v-if="isExpanded" :label="isImmersive ? 'Exit fullscreen' : 'Fullscreen'">
      <button
        type="button"
        :class="ctlClass"
        :aria-label="isImmersive ? 'Exit fullscreen' : 'Show call fullscreen'"
        @click="emit('toggleFullscreen')"
      >
        <component :is="isImmersive ? Minimize : Fullscreen" aria-hidden="true" />
      </button>
    </AppTooltip>
    <div ref="reactionsWrapEl" :class="popWrapClass">
      <AppTooltip label="Reactions">
      <button
        ref="reactionsTriggerEl"
        type="button"
        :class="ctlClass"
        aria-label="Reactions"
        aria-haspopup="menu"
        :aria-expanded="reactionsOpen"
        @click="reactionsOpen ? closeReactions(false) : openReactions(true)"
        @keydown="onReactionsTriggerKeydown"
      >
        <SmilePlus aria-hidden="true" />
      </button>
      </AppTooltip>
      <div
        v-show="reactionsOpen"
        ref="reactionsMenuEl"
        :class="reactionMenuClass"
        role="menu"
        aria-label="Reactions"
        @keydown="onReactionsMenuKeydown"
      >
        <button
          v-for="emoji in reactionEmojis"
          :key="emoji"
          type="button"
          role="menuitem"
          :class="reactionItemClass"
          @click="selectReaction(emoji)"
        >
          {{ emoji }}
        </button>
      </div>
    </div>
    <AppTooltip v-if="raisedHandAvailable" :label="raisedHandActive ? 'Lower hand' : 'Raise hand'">
      <button
        type="button"
        :class="ctlClass"
        :aria-label="raisedHandActive ? 'Lower hand' : 'Raise hand'"
        :aria-pressed="raisedHandActive ? 'true' : 'false'"
        @click="emit('toggleRaisedHand')"
      >
        <Hand aria-hidden="true" />
      </button>
    </AppTooltip>
    <AppTooltip :label="participantsOpen ? 'Close participants' : 'Open participants'">
      <button
        type="button"
        :class="ctlClass"
        :aria-label="participantsOpen ? 'Close participants' : 'Open participants'"
        :aria-pressed="participantsOpen"
        :aria-expanded="participantsOpen"
        @click="emit('toggleParticipants')"
      >
        <Users aria-hidden="true" />
        <span
          v-if="participantCount > 0"
          :class="countClass"
          aria-hidden="true"
        >{{ participantCount }}</span>
      </button>
    </AppTooltip>
    <AppTooltip :label="chatOpen ? 'Close chat' : 'Open chat'">
      <button
        type="button"
        :class="ctlClass"
        :aria-label="chatButtonLabel"
        :aria-pressed="chatOpen"
        :aria-expanded="chatOpen"
        @click="emit('toggleChat')"
      >
        <MessageSquare aria-hidden="true" />
        <span
          v-if="chatUnread > 0"
          :class="unreadClass"
          aria-hidden="true"
        >{{ chatUnread }}</span>
      </button>
    </AppTooltip>
    <AppTooltip v-if="pictureInPictureSupported" :label="pictureInPictureActive ? 'Return to call' : 'Picture-in-Picture'">
      <button
        type="button"
        :class="ctlClass"
        :aria-label="pictureInPictureActive ? 'Return to call' : 'Open Picture-in-Picture'"
        :aria-pressed="pictureInPictureActive"
        @click="emit('togglePictureInPicture')"
      >
        <PictureInPicture2 aria-hidden="true" />
      </button>
    </AppTooltip>
    <div ref="moreWrapEl" :class="popWrapClass">
      <button
        ref="moreTriggerEl"
        type="button"
        :class="ctlClass"
        aria-label="More options"
        aria-haspopup="menu"
        :aria-expanded="moreOpen"
        @click="moreOpen ? closeMore(false) : openMore(true)"
        @keydown="onMoreTriggerKeydown"
      >
        <MoreHorizontal aria-hidden="true" />
      </button>
      <div
        v-show="moreOpen"
        ref="moreMenuEl"
        :class="moreMenuClass"
        role="menu"
        aria-label="More options"
        @keydown="onMoreMenuKeydown"
      >
        <button
          type="button"
          role="menuitemcheckbox"
          :aria-checked="!selfViewHidden"
          :class="moreItemClass"
          @click="selectSelfView"
        >
          <ScanFace aria-hidden="true" />
          <span>Self-view</span>
          <Check v-if="!selfViewHidden" data-check aria-hidden="true" />
        </button>
        <button
          type="button"
          role="menuitem"
          :class="moreItemClass"
          @click="selectSettings"
        >
          <Settings aria-hidden="true" />
          <span>Call settings</span>
        </button>
      </div>
    </div>

    <span :class="dividerClass" aria-hidden="true" />

    <AppTooltip label="Hang up">
      <button
        type="button"
        :class="hangupClass"
        @click="emit('hangup')"
      >
        <PhoneOff aria-hidden="true" />
        <span :class="hangupLabelClass">Hang up</span>
      </button>
    </AppTooltip>
  </div>
</template>
