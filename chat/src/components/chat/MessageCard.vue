<script setup lang="ts">
import { timelineRowKey } from "@/lib/timeline-row-key";
import { ref, computed, watch } from "vue";
import {
  AlertCircle,
  Clock,
  Loader2,
  MoreHorizontal,
  CornerDownRight,
  MessageSquare,
  MessagesSquare,
  PhoneCall,
  Reply,
  Pin,
  PinOff,
  Trash2,
} from "lucide-vue-next";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import CallAnchorCard from "@/components/calls/CallAnchorCard.vue";
import MessageBody from "@/components/chat/MessageBody.vue";
import MessageActionSheet from "@/components/chat/MessageActionSheet.vue";
import MessageEditForm from "@/components/chat/MessageEditForm.vue";
import MessageHoverToolbar from "@/components/chat/MessageHoverToolbar.vue";
import MessageReactionChips from "@/components/chat/MessageReactionChips.vue";
import MessageSafetyScores from "@/components/chat/MessageSafetyScores.vue";
import MessageSystemBand from "@/components/chat/MessageSystemBand.vue";
import type {
  TimelineMessage,
  ExtensionAnnotationAction,
  MarkupSpan,
  MessageReference,
} from "@/lib/chat-ui";
import { messageMentionsBareJid } from "@/lib/mentions";
import { jidLocalpart, resourceOf } from "@/lib/xmpp/jid";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { OccupantAuthority, OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatTimelineTimeOfDay } from "@/channels/timeline";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import { callThreadAnchorLabel, callThreadAnchorThreadId, useCallAnchorCardState } from "@/lib/call-thread-anchor";
import { resolveThreadActionTarget } from "@/lib/thread-action-target";
import type { CallMedia } from "@/lib/calls/types";
import { authorBadge as authorBadgeFor, authorBadgeTooltip as authorBadgeTooltipFor, identityHats } from "./message-card-badges";
import { eventBandsFor, rendersAsSystemBand } from "./message-system-band";
import { visibleQuestionScore, visibleSafetyScores } from "./message-safety-scores";
import { formatThreadRecency } from "./message-thread-recency";
import { threadReplyCardLabel, threadReplyCountLabel, type ThreadLatestReply } from "./thread-reply-card";
import {
  hasActiveTextSelection,
  prefersCoarsePointer,
  shouldOpenThreadOnRowTap,
  tapTargetIsInteractive,
} from "./message-row-tap";
import { useMessageActionSurfaces } from "./composables/use-message-action-surfaces";
import { useMessageGestures } from "./composables/use-message-gestures";
import { css, cx } from "styled-system/css";
import { kicker, msg } from "styled-system/recipes";

const props = defineProps<{
  message: TimelineMessage;
  currentUser?: string;
  currentUserJid?: string;
  hats: OccupantHat[];
  /** XEP-0045 affiliation/role for the message's author. Drives the
   * OWNER / ADMIN / MOD chip on the meta row. Distinct from `hats`,
   * which carries XEP-0317 descriptive metadata only. */
  authority?: OccupantAuthority | null;
  /** Real bare JID behind the author, for the avatar; `null` = initials. */
  avatarJid?: string | null;
  presence?: OccupantPresence;
  lastSeen?: number;
  authorJid?: string;
  threadReplyCount?: number;
  /** Unique participants in this thread (capped, current user excluded
   * by the caller). Rendered as a tiny avatar stack on the thread chip
   * so the eye can triage threads without opening them. */
  threadParticipants?: { key: string; nick: string; jid: string | null; presence: OccupantPresence }[];
  /** ISO timestamp of the most-recent reply in this thread, used to
   * suffix the card's count with a relative-time hint ("· 2 min ago"). */
  threadLastReplyAt?: string;
  /** Newest loaded reply, for the card's "author: snippet" line; absent
   * when the count is known but no reply is loaded. */
  threadLatestReply?: ThreadLatestReply | null;
  hideThreadChip?: boolean;
  /**
   * Unconditional kill switch for the inline reply-to chip. The thread
   * panel sets this true on every message it renders — once you're inside
   * a thread, replies-to-the-root are implicit and replies-to-replies are
   * still inside the thread context, so the chip adds no information.
   */
  hideReplyChip?: boolean;
  grouped?: boolean;
  reactionModeSelected?: boolean;
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>;
  /** #414: this message is pinned in its host room. */
  isPinned?: boolean;
  /** #414: current user is room Owner or Admin (controls pin/unpin
   * action-sheet entry visibility). */
  canPinMessages?: boolean;
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
  /**
   * Overrides the toolbar/action-sheet/swipe thread target. ThreadPanel uses
   * this on reply rows so "Reply in thread" enters the reply's child thread
   * instead of reopening the parent thread carried by message.threadId.
   */
  threadActionThreadId?: string;
  callRoomJid?: string | null;
  callChannelId?: string | null;
  hideCallAnchorCard?: boolean;
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[], linkPreview?: ComposerLinkPreviewSendPayload];
  retract: [messageId: string];
  react: [messageId: string, emoji: string];
  reply: [message: TimelineMessage];
  scrollToMessage: [messageId: string];
  avatarClick: [author: string, jid: string | null];
  openThread: [threadId: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  pin: [messageId: string];
  unpin: [messageId: string];
}>();

const rowHats = computed(() => identityHats(props.hats, props.avatarJid));
const authorBadge = computed(() => authorBadgeFor(props.authority, rowHats.value));
const safetyScores = computed(() => {
  const scores = props.message.safetyScores;
  if (!scores || props.message.isRetracted) return null;
  return visibleSafetyScores(props.message) || visibleQuestionScore(props.message) ? scores : null;
});
const authorBadgeTooltip = computed(() => authorBadgeTooltipFor(props.authority, rowHats.value));

const eventBands = computed(() => eventBandsFor(props.message.extensionAnnotations));
const renderAsSystemBand = computed(() => rendersAsSystemBand(eventBands.value));

const replyAuthorName = computed(() => {
  const author = props.message.replyTo?.author;
  if (!author) return "";
  return author.includes("/") ? resourceOf(author) || author : jidLocalpart(author);
});

const deliveryStatusLabel = computed(() => {
  switch (props.message.deliveryStatus) {
    case "queued":
      return "queued";
    case "sending":
      return "sending…";
    case "rejected":
    case "failed":
      return "failed";
    default:
      return null;
  }
});

const deliveryStatusTone = computed(() => {
  switch (props.message.deliveryStatus) {
    case "queued":
      return "warning";
    case "rejected":
    case "failed":
      return "danger";
    default:
      return undefined;
  }
});

const deliveryStatusIcon = computed(() => {
  switch (props.message.deliveryStatus) {
    case "queued":
      return Clock;
    case "sending":
      return Loader2;
    case "rejected":
    case "failed":
      return AlertCircle;
    default:
      return null;
  }
});

const replyChipExpanded = ref(false);

function onReplyChipClick() {
  const replyTo = props.message.replyTo;
  if (!replyTo) return;
  if (replyTo.preview) {
    replyChipExpanded.value = !replyChipExpanded.value;
  }
  // If this message lives inside a thread, also open the thread panel so the
  // parent is reachable even though thread children are hidden from the feed.
  if (props.message.threadId) {
    emit("openThread", props.message.threadId);
  }
  emit("scrollToMessage", replyTo.id);
}

const threadReplyCountValue = computed(() => props.threadReplyCount ?? 0);
const showThreadChip = computed(
  () => !props.hideThreadChip && threadReplyCountValue.value > 0,
);

function openThreadFromChip() {
  emit("openThread", props.message.id);
}

const MAX_CHIP_PARTICIPANTS = 3;
const visibleThreadParticipants = computed(() =>
  (props.threadParticipants ?? []).slice(0, MAX_CHIP_PARTICIPANTS),
);
const threadParticipantOverflow = computed(() => {
  const total = props.threadParticipants?.length ?? 0;
  return Math.max(0, total - visibleThreadParticipants.value.length);
});

const threadChipRecency = computed(() => formatThreadRecency(props.threadLastReplyAt));
const threadCountLabel = computed(() => threadReplyCountLabel(threadReplyCountValue.value));
const threadCardLabel = computed(() =>
  threadReplyCardLabel({
    count: threadReplyCountValue.value,
    latest: props.threadLatestReply ?? null,
    recency: threadChipRecency.value,
  }),
);
const threadActionTargetId = computed(() => resolveThreadActionTarget(props.message, props.threadActionThreadId));
const callThreadLabel = computed(() => callThreadAnchorLabel(props.message));
const callThreadId = computed(() => callThreadAnchorThreadId(props.message));
const callAnchorCardState = useCallAnchorCardState(
  () => props.message,
  () => props.callRoomJid,
  () => threadReplyCountValue.value,
);

function openCallThreadAnchor() {
  if (!callThreadId.value) return;
  emit("openThread", callThreadId.value);
}

function joinCallThreadAnchor() {
  const state = callAnchorCardState.value;
  // MUC-only: `joinChannelCall` joins a group call by room JID. DM anchors
  // never offer Join (their card has no action), but guard anyway so a DM
  // anchor — whose `callRoomJid` is the peer JID — can't fire a malformed
  // group-call join.
  if (props.message.callThread?.kind !== "muc") return;
  if (!state || !props.callRoomJid || state.status !== "live") return;
  emit("joinChannelCall", props.callChannelId ?? null, props.callRoomJid, state.media);
}

function togglePinFromMenu() {
  if (!props.message.id) return;
  if (props.isPinned) {
    emit("unpin", props.message.id);
  } else {
    emit("pin", props.message.id);
  }
  closeSheet();
}

function startReplyInThreadFromMenu() {
  // Open the target thread first so the panel composer sends with the
  // matching threadOverride. ThreadPanel can target a reply's own id here,
  // which starts an empty child thread whose parent is the current thread.
  emit("openThread", threadActionTargetId.value);
  closeSheet();
}

const isMentioned = computed(() => {
  return messageMentionsBareJid(props.message, props.currentUserJid);
});
const isForumTopic = computed(() => props.message.forumPostKind === "topic" && !!props.message.forumTitle);
const isForumReply = computed(() => props.message.forumPostKind === "reply");
// XEP-0461 §3.2: groupchat replies require the room-assigned XEP-0359
// stanza-id. Hide the reply action on messages that lack one rather than
// surface a button that will refuse on click.
const canReplyToMessage = computed(() => !!props.message.replyableId);
const forumThreadLabel = computed(() =>
  props.message.forumPostKind === "topic"
    ? props.message.forumTitle
    : props.message.forumThreadTitle,
);

const isEditing = ref(false);

function startEdit() {
  isEditing.value = true;
}

function closeEdit() {
  isEditing.value = false;
}

function submitEdit(
  newBody: string,
  markup?: MarkupSpan[],
  references?: MessageReference[],
  linkPreview?: ComposerLinkPreviewSendPayload,
) {
  emit("edit", props.message.id, newBody, markup, references, linkPreview);
}

function emitAvatarClick() {
  emit("avatarClick", props.message.author, props.avatarJid ?? null);
}

const bubbleEl = ref<HTMLElement | null>(null);

const {
  pickerOpen,
  sheetOpen,
  desktopToolbarLockedByAnother,
  desktopToolbarVisibilityClass,
  closePicker,
  closeSheet,
  openSheet,
  togglePicker,
} = useMessageActionSurfaces({
  messageId: () => props.message.id,
  reactionModeSelected: () => props.reactionModeSelected ?? false,
  bubbleEl,
});

function react(emoji: string) {
  emit("react", props.message.id, emoji);
  closePicker(true);
  closeSheet();
}

function startReplyFromMenu() {
  emit("reply", props.message);
  closePicker(true);
  closeSheet();
}

function startEditFromMenu() {
  startEdit();
  closePicker(true);
  closeSheet();
}

function retractFromMenu() {
  emit("retract", props.message.id);
  closePicker(true);
  closeSheet();
}

const gestures = useMessageGestures({
  onLongPress: openSheet,
  onSwipeLeft: () => {
    // Right-to-left drag opens (or enters) the thread for this message.
    // ThreadPanel can override the target so swiping a reply enters that
    // reply's child thread instead of reopening its parent thread.
    emit("openThread", threadActionTargetId.value);
  },
  onSwipeRight: () => {
    // Left-to-right drag fills the composer reply chip targeting this
    // message — same path as the toolbar reply button.
    emit("reply", props.message);
  },
});
const swipe = gestures.swipe;

// Touch only: a plain tap on a post with replies opens its thread. Fine
// pointers keep text selection and use the reply card, which is also the
// keyboard path, so the row stays a non-interactive element (no role, no
// tab stop). Decided on the click the browser sends after a stationary
// single-finger tap: a tap that only stops momentum scrolling gets no
// click and opens nothing. Rows inside the thread pane hide the card, so
// they never open a thread from a tap. The listener is attached in script
// because it is a touch-tap refinement of the row, not a mouse affordance.
function onRowClick(event: MouseEvent) {
  const row = bubbleEl.value;
  // Always consume the press state so a stale tap is never reused.
  const stationaryTouch = gestures.consumeStationaryTap();
  if (!row || isEditing.value || sheetOpen.value) return;
  const open = shouldOpenThreadOnRowTap({
    opensThread: showThreadChip.value,
    coarsePointer: prefersCoarsePointer(),
    stationaryTouch,
    hasTextSelection: hasActiveTextSelection(),
    targetIsInteractive: tapTargetIsInteractive(event.target, row),
  });
  if (open) emit("openThread", props.message.id);
}

const isPending = computed(() => props.message.deliveryStatus === "sending" || props.message.deliveryStatus === "queued");
const swipeArmed = computed(() => {
  if (!swipe.isArmed.value) return undefined;
  return swipe.direction.value === -1 ? "thread" : "reply";
});

// --- Presentation (Polar Huddle): every conversation uses the `msg` row.
// Row states ride on data attributes so the swipe hints, grouped time
// gutter and touch trigger can follow them.
const c = {
  row: css({
    width: "100%",
    alignItems: "start",
    touchAction: "pan-y",
    transition: "background-color {durations.fast} {easings.out}, transform 180ms {easings.out}",
    _motionReduce: { transition: "none" },
    _coarse: { pr: "52px" },
    "@media (pointer: fine)": { touchAction: "auto" },
    "&[data-swiping]": { transition: "none", willChange: "transform" },
  }),
  pending: css({ opacity: 0.5 }),
  selected: css({ bg: "state.selected", boxShadow: "inset 0 0 0 1px {colors.border.strong}" }),
  forum: css({ bg: "surface", border: "1px solid {colors.border.soft}", py: "3", _hover: { bg: "surface" } }),
  quiet: css({ color: "fg.muted" }),
  retracted: css({ opacity: 0.55 }),
  iconCell: css({ display: "flex", alignItems: "center", justifyContent: "center", height: "24px", color: "fg.soft", "& svg": { width: "16px", height: "16px" } }),
  avatarBtn: css({
    display: "flex",
    p: 0,
    border: 0,
    bg: "transparent",
    borderRadius: "pill",
    cursor: "pointer",
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
  }),
  gutter: css({
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    height: "24px",
    textStyle: "timestamp",
    fontSize: "2xs",
    color: "fg.muted",
    fontVariantNumeric: "tabular-nums",
    whiteSpace: "nowrap",
    overflow: "hidden",
    opacity: 0,
    transition: "opacity {durations.fast} {easings.out}",
    "[data-msg-row]:hover &, [data-msg-row]:focus-within &": { opacity: 1 },
    _coarse: { "[data-msg-row]:active &": { opacity: 1 } },
  }),
  stack: css({ position: "relative", display: "flex", minWidth: 0, flexDirection: "column", alignItems: "flex-start", gap: "1", "& > [data-fill]": { alignSelf: "stretch" } }),
  meta: css({ minWidth: 0, maxWidth: "100%", overflow: "hidden", whiteSpace: "nowrap" }),
  authorBtn: css({
    minWidth: 0,
    maxWidth: "min(20rem, 60vw)",
    overflow: "hidden",
    textOverflow: "ellipsis",
    p: 0,
    border: 0,
    bg: "transparent",
    color: "fg",
    font: "inherit",
    fontWeight: 700,
    textAlign: "left",
    cursor: "pointer",
    textDecoration: "underline",
    textDecorationColor: "transparent",
    textUnderlineOffset: "2px",
    transition: "text-decoration-color {durations.fast} {easings.out}",
    _hover: { textDecorationColor: "currentColor" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px", borderRadius: "xs" },
  }),
  badge: css({
    flex: "none",
    fontSize: "10px",
    fontWeight: 700,
    letterSpacing: "0.06em",
    textTransform: "uppercase",
    lineHeight: 1,
    color: "fg.muted",
    "&[data-tone=warning]": { color: "warning.text" },
    "&[data-tone=action]": { color: "action.text" },
    "&[data-tone=success]": { color: "success.text" },
  }),
  status: css({
    display: "inline-flex",
    flex: "none",
    alignItems: "center",
    gap: "1",
    textStyle: "timestamp",
    color: "fg.soft",
    "& svg": { width: "12px", height: "12px" },
    "&[data-tone=warning]": { color: "warning.text" },
    "&[data-tone=danger]": { color: "danger.text" },
  }),
  deleted: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1.5",
    fontStyle: "italic",
    color: "fg.muted",
    "& svg": { width: "14px", height: "14px", flex: "none", opacity: 0.7 },
  }),
  pinEvent: css({ fontSize: "sm", fontStyle: "italic", color: "fg.muted", "& > span": { ml: "2", fontStyle: "normal" } }),
  timeInline: css({ textStyle: "timestamp", color: "fg.muted", fontVariantNumeric: "tabular-nums" }),
  callFallback: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1.5",
    p: 0,
    border: 0,
    bg: "transparent",
    fontSize: "sm",
    color: "fg.muted",
    cursor: "pointer",
    _hover: { color: "fg" },
    _disabled: { cursor: "default" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
  }),
  forumTopic: css({ display: "flex", flexDirection: "column", gap: "1", width: "100%", "& h3": { m: 0, font: "700 18px/1.3 {fonts.display}", color: "fg" } }),
  forumKicker: cx(kicker(), css({ color: "action.text" })),
  forumReply: css({
    display: "flex",
    alignItems: "center",
    gap: "1.5",
    minHeight: "28px",
    px: "2",
    borderRadius: "sm",
    bg: "surface.2",
    textStyle: "caption",
    color: "fg.muted",
    "& svg": { width: "12px", height: "12px", flex: "none", color: "action.text" },
    "& span": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  }),
  replyChip: css({
    display: "flex",
    alignItems: "center",
    gap: "1.5",
    minHeight: "28px",
    maxWidth: "100%",
    px: "2",
    border: 0,
    borderLeft: "3px solid {colors.action.text}",
    borderRadius: "sm",
    bg: "surface.2",
    textStyle: "caption",
    color: "fg.muted",
    textAlign: "left",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}, color {durations.fast} {easings.out}",
    _hover: { bg: "state.hover", color: "fg" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
    "& > svg": { width: "12px", height: "12px", flex: "none", color: "action.text" },
  }),
  replyAuthor: css({ fontWeight: 700, color: "action.text" }),
  replyPreview: css({ flex: 1, minWidth: 0, fontStyle: "italic", opacity: 0.8, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  replyPreviewOpen: css({ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }),
  replyId: css({ fontFamily: "mono", opacity: 0.6 }),
  threadCard: css({
    display: "flex",
    flexDirection: "column",
    alignItems: "flex-start",
    gap: "1",
    maxWidth: "100%",
    mt: "2",
    px: "2",
    py: "1",
    ml: "-2",
    border: 0,
    borderRadius: "sm",
    bg: "transparent",
    textAlign: "left",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}",
    _hover: { bg: "state.hover" },
    _active: { bg: "state.press" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
    "[data-own] &": { ml: 0, mr: "-2", alignItems: "flex-end" },
  }),
  threadSummary: css({
    display: "inline-flex",
    minWidth: 0,
    maxWidth: "100%",
    alignItems: "center",
    gap: "1.5",
    fontSize: "xs",
    color: "action.text",
    whiteSpace: "nowrap",
    "& > svg": { width: "16px", height: "16px", flex: "none" },
  }),
  threadFaces: css({ display: "inline-flex", flex: "none", alignItems: "center", isolation: "isolate" }),
  threadFace: css({ display: "inline-flex", ml: "-1.5", borderRadius: "pill", outline: "2px solid {colors.bg}", _first: { ml: 0 } }),
  threadOverflow: css({
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    minWidth: "20px",
    height: "20px",
    px: "1",
    ml: "-1",
    borderRadius: "pill",
    bg: "surface.2",
    outline: "2px solid {colors.bg}",
    fontSize: "10px",
    fontWeight: 700,
    color: "fg.muted",
    fontVariantNumeric: "tabular-nums",
  }),
  threadMeta: css({ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis" }),
  threadCount: css({ fontWeight: 700 }),
  threadRecency: css({ color: "fg.muted", fontVariantNumeric: "tabular-nums" }),
  threadLatest: css({ maxWidth: "100%", minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: "sm", color: "fg.muted", "& > span": { fontWeight: 700, color: "fg" } }),
  hint: css({
    position: "absolute",
    top: "50%",
    display: "none",
    alignItems: "center",
    justifyContent: "center",
    width: "24px",
    height: "24px",
    borderRadius: "pill",
    border: "1px solid {colors.action.text}",
    bg: "action.subtle",
    color: "action.text",
    translate: "0 -50%",
    scale: "0.7",
    opacity: 0.16,
    pointerEvents: "none",
    transition: "opacity {durations.fast} {easings.out}, scale {durations.fast} {easings.out}",
    _coarse: { display: "flex" },
    "& svg": { width: "12px", height: "12px" },
    "[data-swiping] > &": { opacity: 0.6 },
  }),
  hintReply: css({ left: "-52px", "[data-swiping][data-swipe-armed=reply] > &": { opacity: 1, scale: "1" } }),
  hintThread: css({ right: "-52px", "[data-swiping][data-swipe-armed=thread] > &": { opacity: 1, scale: "1" } }),
  trigger: css({
    position: "absolute",
    top: "1",
    right: "1",
    zIndex: "sticky",
    display: "none",
    alignItems: "center",
    justifyContent: "center",
    width: "44px",
    height: "44px",
    border: 0,
    borderRadius: "control",
    bg: "transparent",
    color: "fg.muted",
    opacity: 0.22,
    cursor: "pointer",
    transition: "opacity {durations.fast} {easings.out}, background-color {durations.fast} {easings.out}",
    _coarse: { display: "flex" },
    _hover: { color: "fg", bg: "state.hover" },
    _active: { bg: "state.press", opacity: 1 },
    "&[aria-expanded=true], [data-msg-row]:focus-within > &, [data-msg-row]:active > &": { opacity: 1 },
    "& svg": { width: "20px", height: "20px" },
  }),
};

const statusBits = computed(() => {
  const bits: { key: string; icon?: typeof Pin; text?: string; label?: string; title?: string; tone?: string }[] = [];
  if (props.isPinned) bits.push({ key: "pin", icon: Pin, label: "Pinned in this channel", title: "Pinned in this channel" });
  if (props.message.isEdited) bits.push({ key: "edited", text: "(edited)" });
  if (props.message.isSelf && deliveryStatusLabel.value) {
    bits.push({ key: "delivery", icon: deliveryStatusIcon.value ?? undefined, text: deliveryStatusLabel.value, tone: deliveryStatusTone.value });
  }
  if (props.message.isSelf && props.message.readBy && props.message.readBy.length > 0) {
    bits.push({ key: "read", text: `Read by ${props.message.readBy.length}`, title: props.message.readBy.join(", ") });
  }
  return bits;
});


watch(bubbleEl, (row, previous) => {
  previous?.removeEventListener("click", onRowClick);
  row?.addEventListener("click", onRowClick);
});
</script>

<template>
  <!-- #414: pin-event system message rendered distinctly from user
       posts (no avatar, italic, muted) so the channel timeline
       reads as "alice pinned a message" without looking like a chat
       reply. -->
  <div
    v-if="message.isPinEvent"
    :data-message-id="message.id"
    :data-message-row-key="timelineRowKey(message)"
    :data-message-created-at="message.createdAt"
    :class="msg({ cont: true })"
  >
    <div :class="c.iconCell">
      <component :is="message.pinEventAction === 'unpinned' ? PinOff : Pin" aria-hidden="true" />
    </div>
    <p :class="c.pinEvent">
      {{ message.body }}
      <span :class="c.timeInline">{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
    </p>
  </div>

  <!-- System band: bot-authored messages carrying event-intent extension
       annotations (GitHub workflow runs, deploy notifications, …).
       Rendered full-width and flat with no avatar gutter — these aren't
       chat replies from a person, they're notifications, and they
       should look like one. The bot's literal body text is suppressed
       because the structured payload already says it better. -->
  <template v-else-if="renderAsSystemBand">
    <MessageSystemBand
      v-for="card in eventBands"
      :key="`band:${card.annotation.extensionId}:${card.annotation.annotationId}`"
      :card="card"
      :message-id="message.id"
      :data-message-row-key="timelineRowKey(message)"
      :message-created-at="message.createdAt"
      :message-author="message.author"
      :invoke-extension-action="invokeExtensionAction"
    />
  </template>

  <div
    v-else-if="message.callThread && !hideCallAnchorCard"
    :data-message-id="message.id"
    :data-message-row-key="timelineRowKey(message)"
    :data-message-created-at="message.createdAt"
    :class="msg()"
  >
    <div :class="c.iconCell">
      <PhoneCall aria-hidden="true" />
    </div>
    <div :class="c.stack">
      <CallAnchorCard
        v-if="callAnchorCardState"
        :state="callAnchorCardState"
        @join="joinCallThreadAnchor"
        @open-thread="openCallThreadAnchor"
      />
      <button
        v-else
        type="button"
        :class="c.callFallback"
        :disabled="!callThreadId"
        @click="openCallThreadAnchor"
      >
        <span>{{ callThreadLabel }}</span>
        <span :class="c.timeInline">{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
      </button>
    </div>
  </div>

  <!-- Retracted tombstone — body is gone but author/time/avatar
       stay for context, faded, with a Trash2 glyph beside the copy. -->
  <div
    v-else-if="message.isRetracted"
    :data-message-id="message.id"
    :data-message-row-key="timelineRowKey(message)"
    :data-message-created-at="message.createdAt"
    data-msg-row
    :class="cx(msg({ cont: grouped }), c.retracted)"
  >
    <div v-if="grouped" :class="c.gutter">
      <span>{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
    </div>
    <UserAvatar
      v-else
      :name="message.author"
      :jid="avatarJid"
      :presence="presence"
      :last-seen="lastSeen"
      size="message"
    />
    <div :class="c.stack">
      <div v-if="!grouped" :class="cx('wd-msg__meta', c.meta)">
        <button
          type="button"
          :class="cx('wd-msg__author', c.authorBtn)"
          :aria-label="`${message.author}, open profile`"
          @click.stop="emitAvatarClick"
        >{{ message.author }}</button>
        <span class="wd-msg__time">
          {{ formatTimelineTimeOfDay(message.createdAt) }}
        </span>
      </div>
      <p :class="c.deleted">
        <Trash2 aria-hidden="true" />
        <span>This message was deleted.</span>
      </p>
    </div>
  </div>

  <!-- Normal message: a room row (`msg`). -->
  <div
    v-else
    ref="bubbleEl"
    :data-message-id="message.id"
    :data-message-row-key="timelineRowKey(message)"
    :data-message-created-at="message.createdAt"
    :data-sheet-open="sheetOpen ? 'true' : 'false'"
    data-msg-row
    :data-swiping="swipe.isSwiping.value || undefined"
    :data-swipe-armed="swipeArmed"
    :class="cx(
      msg({ cont: grouped, mention: isMentioned, new: message.deliveryStatus === 'sending' }),
      c.row,
      isForumTopic && c.forum,
      isPending && c.pending,
      reactionModeSelected && c.selected,
    )"
    :style="swipe.isSwiping.value
      ? {
          '--chat-swipe-x': swipe.translateX.value + 'px',
          transform: `translateX(${swipe.translateX.value}px)`,
        }
      : undefined"
    @pointerdown="gestures.handlers.onPointerdown"
    @pointermove="gestures.handlers.onPointermove"
    @pointerup="gestures.handlers.onPointerup"
    @pointercancel="gestures.handlers.onPointercancel"
    @pointerleave="gestures.handlers.onPointerleave"
    @contextmenu="gestures.handlers.onContextMenu"
  >
    <span :class="cx(c.hint, c.hintReply)" aria-hidden="true"><Reply /></span>
    <span :class="cx(c.hint, c.hintThread)" aria-hidden="true"><MessageSquare /></span>
    <div v-if="grouped" :class="c.gutter" aria-hidden="true">
      <span>{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
    </div>
    <button
      v-else
      :class="c.avatarBtn"
      type="button"
      :aria-label="`${message.author}, open profile`"
      @click.stop="emitAvatarClick"
    >
      <UserAvatar :name="message.author" :jid="avatarJid" :presence="presence" :last-seen="lastSeen" size="message" />
    </button>
    <div :class="c.stack">
      <div v-if="!grouped" :class="cx('wd-msg__meta', c.meta)">
        <button
          type="button"
          :class="cx('wd-msg__author', c.authorBtn)"
          :aria-label="`${message.author}, open profile`"
          @click.stop="emitAvatarClick"
        >{{ message.author }}</button>
        <span
          v-if="authorBadge"
          :class="c.badge"
          :data-tone="authorBadge.tone"
          :title="authorBadgeTooltip"
        >{{ authorBadge.label }}</span>
        <span class="wd-msg__time">
          {{ formatTimelineTimeOfDay(message.createdAt) }}
        </span>
        <span
          v-for="bit in statusBits"
          :key="bit.key"
          :class="c.status"
          :data-tone="bit.tone"
          :title="bit.title"
          :aria-label="bit.label"
        >
          <component :is="bit.icon" v-if="bit.icon" aria-hidden="true" />
          <template v-if="bit.text">{{ bit.text }}</template>
        </span>
      </div>

    <div
      v-if="isForumTopic && forumThreadLabel"
      :class="c.forumTopic"
      data-fill
    >
      <div :class="c.forumKicker">
        Topic
      </div>
      <h3>
        {{ forumThreadLabel }}
      </h3>
    </div>
    <div
      v-else-if="isForumReply && forumThreadLabel"
      :class="c.forumReply"
      data-fill
    >
      <CornerDownRight aria-hidden="true" />
      <span>In {{ forumThreadLabel }}</span>
    </div>
    <!-- Reply preview chip. Clicking scrolls to the parent message; if the
         preview is available we also expand it inline so users still see the
         full quoted text even when the parent has scrolled off-screen or
         hasn't loaded from history yet. -->
    <div v-if="message.replyTo && !hideReplyChip" data-fill>
      <AppTooltip :label="message.replyTo.preview ? 'Show full quoted message and jump to it' : 'Jump to replied message'">
      <button
        type="button"
        :class="c.replyChip"
        :aria-expanded="replyChipExpanded"
        @click="onReplyChipClick"
      >
        <CornerDownRight aria-hidden="true" />
        <span :class="c.replyAuthor">@{{ replyAuthorName }}</span>
        <span
          v-if="message.replyTo.preview"
          :class="cx(c.replyPreview, replyChipExpanded && c.replyPreviewOpen)"
        >{{ message.replyTo.preview }}</span>
        <span v-else :class="c.replyId">{{ message.replyTo.id.slice(0, 8) }}</span>
      </button>
      </AppTooltip>
    </div>

    <!-- Edit mode -->
    <MessageEditForm
      v-if="isEditing"
      :body="message.body"
      :markup="message.markup"
      :references="message.references"
      :link-previews="message.linkPreviews"
      :link-preview-lookup="linkPreviewLookup"
      :link-preview-scope="linkPreviewScope"
      @save="submitEdit"
      @close="closeEdit"
    />

    <MessageBody
      v-else
      :message="message"
      :invoke-extension-action="invokeExtensionAction"
    />

    <MessageReactionChips
      v-if="message.reactions && Object.keys(message.reactions).length > 0"
      :reactions="message.reactions"
      :current-user="currentUser"
      @react="(emoji) => emit('react', message.id, emoji)"
    />

    <MessageSafetyScores
      v-if="safetyScores"
      :scores="safetyScores"
      :message-id="message.id"
      :data-message-row-key="timelineRowKey(message)"
    />

    <!-- Attached reply card: the thread's summary under the post. One
         button; hidden in the thread pane (hideThreadChip), which already
         lists the replies. -->
    <button
      v-if="showThreadChip"
      type="button"
      :class="c.threadCard"
      data-thread-card
      :aria-label="threadCardLabel"
      @click="openThreadFromChip"
    >
      <span :class="c.threadSummary">
        <span
          v-if="visibleThreadParticipants.length > 0"
          :class="c.threadFaces"
          aria-hidden="true"
        >
          <span
            v-for="participant in visibleThreadParticipants"
            :key="`thread-card-avatar:${message.id}:${participant.key}`"
            :class="c.threadFace"
            data-thread-face
          >
            <UserAvatar
              :name="participant.nick"
              :jid="participant.jid"
              :presence="participant.presence"
              size="xs"
            />
          </span>
          <span
            v-if="threadParticipantOverflow > 0"
            :class="c.threadOverflow"
          >+{{ threadParticipantOverflow }}</span>
        </span>
        <MessagesSquare v-else aria-hidden="true" />
        <span :class="c.threadMeta">
          <span :class="c.threadCount">{{ threadCountLabel }}</span>
          <span v-if="threadChipRecency" :class="c.threadRecency"> · {{ threadChipRecency }}</span>
        </span>
      </span>
      <span v-if="threadLatestReply" :class="c.threadLatest" data-thread-latest>
        <span>{{ threadLatestReply.author }}</span><template v-if="threadLatestReply.snippet">: {{ threadLatestReply.snippet }}</template>
      </span>
    </button>

    <MessageHoverToolbar
      v-if="!isEditing && !desktopToolbarLockedByAnother"
      :picker-open="pickerOpen"
      :visibility-class="desktopToolbarVisibilityClass"
      :reaction-mode-selected="reactionModeSelected ?? false"
      :can-reply="canReplyToMessage"
      :thread-reply-count="threadReplyCountValue"
      :can-pin="!!(canPinMessages && message.id)"
      :is-pinned="isPinned ?? false"
      :is-self="message.isSelf"
      @react="react"
      @toggle-picker="togglePicker"
      @close-picker="closePicker(true)"
      @reply="startReplyFromMenu"
      @reply-in-thread="startReplyInThreadFromMenu"
      @toggle-pin="togglePinFromMenu"
      @edit="startEditFromMenu"
      @retract="retractFromMenu"
    />
    </div>

    <!-- Action-sheet trigger. Touch-only; desktop already has the hover
         toolbar. Quiet at rest so a column of ••• buttons doesn't
         out-shout the messages; full strength while the row is focused
         or pressed, or its sheet is open. -->
    <button
      v-if="!isEditing"
      type="button"
      :class="c.trigger"
      aria-label="Message actions"
      :aria-expanded="sheetOpen"
      aria-haspopup="dialog"
      @click="openSheet"
    >
      <MoreHorizontal aria-hidden="true" />
    </button>
  </div>

  <MessageActionSheet
    :open="sheetOpen"
    :can-reply="canReplyToMessage"
    :thread-reply-count="threadReplyCountValue"
    :can-pin="!!(canPinMessages && message.id)"
    :is-pinned="isPinned ?? false"
    :is-self="message.isSelf"
    @react="react"
    @reply="startReplyFromMenu"
    @reply-in-thread="startReplyInThreadFromMenu"
    @toggle-pin="togglePinFromMenu"
    @edit="startEditFromMenu"
    @retract="retractFromMenu"
    @close="closeSheet"
  />
</template>
