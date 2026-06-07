<script setup lang="ts">
import { useStore } from "@nanostores/vue";
import { ref, computed, nextTick, onBeforeUnmount, watch } from "vue";
import {
  AlertCircle,
  Clock,
  Loader2,
  MoreHorizontal,
  Pencil,
  Reply,
  SmilePlus,
  Trash2,
  CornerDownRight,
  ExternalLink,
  Github,
  LayoutDashboard,
  MessageSquare,
  MessagesSquare,
  PhoneCall,
  Pin,
  PinOff,
  X,
} from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import MessageBody from "@/components/chat/MessageBody.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import {
  extensionPresentation,
  type TimelineMessage,
  type ExtensionAnnotation,
  type ExtensionAnnotationAction,
  type MarkupSpan,
  type MessageReference,
} from "@/lib/chat-ui";
import { useExtensionAnnotationActions } from "@/channels/extension-annotation-actions";
import { messageMentionsBareJid } from "@/lib/mentions";
import { richMessageToTiptap, tiptapToRichMessage } from "@/lib/rich-message";
import { useComposerLinkPreview } from "@/lib/use-composer-link-preview";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { MucAffiliation, MucRole, OccupantAuthority, OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatTimelineTimeOfDay } from "@/channels/timeline";
import { useLongPress } from "@/ui/gestures/long-press";
import { useHorizontalSwipe } from "@/ui/gestures/horizontal-swipe";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import {
  $desktopToolbarOwnerId,
  $desktopToolbarSuppressed,
  $desktopToolbarSuspensionEpoch,
  clearDesktopToolbarOwner,
} from "@/stores/message-toolbar";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";
import { callThreadAnchorLabel, callThreadAnchorThreadId } from "@/lib/call-thread-anchor";

// Two separate badge layers:
//
// 1. **Authority badge** — owner / admin / moderator. Derived from the
//    `authority` prop, which mirrors the XEP-0045 `<x muc#user>` payload
//    on each presence. Server-enforced; clients render it but don't
//    invent it.
//
// 2. **Descriptive badge** — bot / verified / future descriptive hats.
//    Derived from XEP-0317 `<hats>`. Client renders; no protocol
//    semantics.
//
// At most one badge is shown next to the author name to keep the meta
// row breathable. Authority outranks descriptive: if you're an admin
// and a bot, the badge says ADMIN. Profile drawer surfaces show the
// full hat list separately.
interface BadgeView {
  label: string;
  /** Tailwind class for the chip's text colour. */
  colorClass: string;
  /** Stable sort key used to pick the senior badge when multiple
   * candidates exist (e.g., admin + bot). Higher wins. */
  rank: number;
}

function authorityBadge(authority: OccupantAuthority | null | undefined): BadgeView | null {
  const affiliation: MucAffiliation = authority?.affiliation ?? "none";
  const role: MucRole = authority?.role ?? "none";
  if (affiliation === "owner") {
    return { label: "OWNER", colorClass: "text-warning/70", rank: 4 };
  }
  if (affiliation === "admin") {
    return { label: "ADMIN", colorClass: "text-primary/75", rank: 3 };
  }
  // Role=Moderator without owner/admin affiliation is the "promoted
  // for this session" case — still authoritative, still rendered.
  if (role === "moderator") {
    return { label: "MOD", colorClass: "text-primary/75", rank: 2 };
  }
  return null;
}

// Waddle's descriptive hat URIs live under `urn:waddle:hats:*`, not
// `urn:xmpp:hats:*`. XEP-0317 deliberately leaves the hat URI value
// space open; minting Waddle-specific semantics inside the XSF
// reserve would falsely claim XSF registration. The container
// namespace `urn:xmpp:hats:0` is unchanged because that one is
// spec-defined.
const DESCRIPTIVE_HAT_LABELS: Record<string, string> = {
  "urn:waddle:hats:bot": "BOT",
  "urn:waddle:hats:verified": "VERIFIED",
};

const DESCRIPTIVE_HAT_COLORS: Record<string, string> = {
  "urn:waddle:hats:bot": "text-success/75",
  "urn:waddle:hats:verified": "text-primary/75",
};

const DESCRIPTIVE_HAT_RANK: Record<string, number> = {
  "urn:waddle:hats:verified": 1,
  "urn:waddle:hats:bot": 0,
};

function descriptiveBadge(hats: OccupantHat[] | null | undefined): BadgeView | null {
  if (!hats || hats.length === 0) return null;
  let best: BadgeView | null = null;
  for (const hat of hats) {
    const label = DESCRIPTIVE_HAT_LABELS[hat.uri] ?? hat.title;
    const colorClass = DESCRIPTIVE_HAT_COLORS[hat.uri] ?? "text-muted-foreground";
    const rank = DESCRIPTIVE_HAT_RANK[hat.uri] ?? 0;
    if (best === null || rank > best.rank) {
      best = { label, colorClass, rank };
    }
  }
  return best;
}

const props = defineProps<{
  message: TimelineMessage;
  currentUser?: string;
  currentUserJid?: string;
  hats: OccupantHat[];
  /** XEP-0045 affiliation/role for the message's author. Drives the
   * OWNER / ADMIN / MOD chip on the meta row. Distinct from `hats`,
   * which carries XEP-0317 descriptive metadata only. */
  authority?: OccupantAuthority | null;
  avatarUrl?: string | null;
  presence?: OccupantPresence;
  lastSeen?: number;
  authorJid?: string;
  threadReplyCount?: number;
  /** Unique participants in this thread (capped, current user excluded
   * by the caller). Rendered as a tiny avatar stack on the thread chip
   * so the eye can triage threads without opening them. */
  threadParticipants?: { nick: string; avatarUrl?: string | null; presence: OccupantPresence }[];
  /** ISO timestamp of the most-recent reply in this thread, used to
   * suffix the chip with a relative-time hint ("· 2 min ago"). */
  threadLastReplyAt?: string;
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
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[], linkPreview?: ComposerLinkPreviewSendPayload];
  retract: [messageId: string];
  react: [messageId: string, emoji: string];
  reply: [message: TimelineMessage];
  scrollToMessage: [messageId: string];
  avatarClick: [author: string];
  openThread: [threadId: string];
  pin: [messageId: string];
  unpin: [messageId: string];
}>();

const quickEmojis = QUICK_REACTION_EMOJIS;

// Single chip rendered next to the author name. Authority outranks
// descriptive hats so e.g. an admin who is also a bot shows ADMIN —
// authority is the load-bearing fact about that occupant in the room;
// the bot tag is supplementary and visible elsewhere (profile drawer).
const authorBadge = computed<BadgeView | null>(() => {
  const authority = authorityBadge(props.authority);
  const descriptive = descriptiveBadge(props.hats);
  if (authority && descriptive) {
    return authority.rank >= descriptive.rank ? authority : descriptive;
  }
  return authority ?? descriptive;
});

// Tooltip on the chip lists every layer the occupant carries so a
// user hovering "ADMIN" still discovers the bot/verified tags.
const authorBadgeTooltip = computed(() => {
  const labels: string[] = [];
  const authority = authorityBadge(props.authority);
  if (authority) labels.push(authority.label);
  for (const hat of props.hats ?? []) {
    labels.push(DESCRIPTIVE_HAT_LABELS[hat.uri] ?? hat.title);
  }
  return labels.join(" · ");
});

const eventBands = computed(() =>
  (props.message.extensionAnnotations ?? [])
    .map((annotation) => ({ annotation, presentation: extensionPresentation(annotation) }))
    .filter((card) => card.presentation.intent === "event"),
);

// Render as a system band when an event-intent annotation declares the
// `chat-bot` surface. The annotation provider (the extension that
// publishes the event) is the one that knows it's a system-level
// notification, not a human reply — and it now says so explicitly via
// `surfaceKind`, which the WASM codec hydrates from the wire-format
// `surface` attribute on the payload's root element. No author-hat
// fallbacks, no "any event intent" hacks: trust the declaration.
const renderAsSystemBand = computed(() =>
  eventBands.value.some((band) => band.annotation.surfaceKind === "chat-bot"),
);

function systemBandIcon(card: { annotation: ExtensionAnnotation; presentation: ReturnType<typeof extensionPresentation> }) {
  if (card.presentation.kind === "github-event") return Github;
  if (card.annotation.surfaceKind === "chat-bot") return MessageSquare;
  return LayoutDashboard;
}

function systemBandToneClass(tone: string): string {
  if (tone === "success") return "chat-system-band--tone-success";
  if (tone === "danger") return "chat-system-band--tone-danger";
  if (tone === "warning") return "chat-system-band--tone-warning";
  return "";
}

// Per-kind modifier so the stylesheet can tune layout for specific
// payload shapes — github-event cards hide their meta-item labels and
// render as a positional chip strip (branch · commit · event) instead
// of a labeled K/V grid.
function systemBandKindClass(kind: string): string {
  return kind ? `chat-system-band--kind-${kind}` : "";
}

// Branch values look like code (slashes, identifier-style suffixes)
// and deserve the same tabular-mono treatment commit SHAs get.
function systemBandMetaValueClass(label: string): string {
  return label === "Commit" || label === "Branch"
    ? "chat-system-band__meta-value--mono"
    : "";
}

// Reuse the same action-state machine MessageBody uses so loading /
// success / error feedback is consistent across all extension surfaces.
const allExtensionAnnotations = computed(() => props.message.extensionAnnotations ?? []);
const invokeExtensionActionRef = computed(() => props.invokeExtensionAction);
const {
  actionState: systemBandActionState,
  invokeExtension: invokeSystemBandAction,
} = useExtensionAnnotationActions({
  annotations: allExtensionAnnotations,
  invokeExtensionAction: invokeExtensionActionRef,
});

const replyAuthorName = computed(() => {
  const author = props.message.replyTo?.author;
  if (!author) return "";
  const nickPart = author.includes("/") ? author.split("/").pop()! : author.split("@")[0];
  return nickPart ?? author;
});

const deliveryStatusLabel = computed(() => {
  switch (props.message.deliveryStatus) {
    case "queued":
      return "queued";
    case "sending":
      return "sending…";
    case "failed":
      return "failed";
    default:
      return null;
  }
});

const deliveryStatusClass = computed(() => {
  switch (props.message.deliveryStatus) {
    case "queued":
      return "text-warning/80";
    case "failed":
      return "text-destructive/80";
    default:
      return "text-muted-foreground/50";
  }
});

const deliveryStatusIcon = computed(() => {
  switch (props.message.deliveryStatus) {
    case "queued":
      return Clock;
    case "sending":
      return Loader2;
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

const showThreadChip = computed(
  () => !props.hideThreadChip && (props.threadReplyCount ?? 0) > 0,
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

function formatThreadRecency(iso: string | undefined): string {
  if (!iso) return "";
  const ms = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(ms) || ms < 0) return "";
  const seconds = Math.floor(ms / 1000);
  if (seconds < 45) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 2) return "1 min ago";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 2) return "1 hour ago";
  if (hours < 24) return `${hours} hours ago`;
  const days = Math.floor(hours / 24);
  if (days < 2) return "1 day ago";
  if (days < 7) return `${days} days ago`;
  return new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

const threadChipRecency = computed(() => formatThreadRecency(props.threadLastReplyAt));
const callThreadLabel = computed(() => callThreadAnchorLabel(props.message));
const callThreadId = computed(() => callThreadAnchorThreadId(props.message));

function openCallThreadAnchor() {
  if (!callThreadId.value) return;
  emit("openThread", callThreadId.value);
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
  // Open the thread first so the follow-up reply lands in the panel's
  // composer. Panel ownership of the reply target means we just need to be
  // sure the panel is in focus; the user can then tap "Reply" in-thread.
  const threadId = props.message.threadId ?? props.message.id;
  emit("openThread", threadId);
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
const editInitialContent = ref<JSONContent | undefined>(undefined);
const editEditorRef = ref<InstanceType<typeof ChatEditor> | null>(null);
const editDraft = ref("");
const isSubmittingEdit = ref(false);
const setEditEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editEditorRef.value = instance;
};
const editTiptapEditor = computed(() => {
  const e = editEditorRef.value as any;
  return e?.editor?.value ?? e?.editor ?? null;
});
const editOriginalRich = computed(() =>
  tiptapToRichMessage(richMessageToTiptap({
    body: props.message.body,
    markup: props.message.markup,
    references: props.message.references,
  })),
);
const editOriginalBody = computed(() => editOriginalRich.value.body.trim());
const editOriginalHasPreview = computed(() => (props.message.linkPreviews?.length ?? 0) > 0);
const editOriginalPreviewUrl = computed(() => props.message.linkPreviews?.[0]?.originalUrl ?? null);
const editLinkPreview = useComposerLinkPreview(
  editDraft,
  computed(() => isEditing.value ? props.linkPreviewLookup : null),
  computed(() => props.linkPreviewScope),
);

function startEdit() {
  const content = richMessageToTiptap({
    body: props.message.body,
    markup: props.message.markup,
    references: props.message.references,
  });
  editInitialContent.value = content;
  editDraft.value = props.message.body;
  isEditing.value = true;
  void nextTick(() => editEditorRef.value?.focus());
}

function cancelEdit() {
  isEditing.value = false;
  isSubmittingEdit.value = false;
  editInitialContent.value = undefined;
  editDraft.value = "";
}

function updateEditDraft(doc: JSONContent) {
  editDraft.value = tiptapToRichMessage(doc).body;
}

async function submitEditFromEditor(doc: JSONContent) {
  if (isSubmittingEdit.value) return;
  const { body, markup, references } = tiptapToRichMessage(doc);
  const draftAtSubmit = editDraft.value;
  const trimmed = body.trim();
  const originalPreviewUrl = editOriginalPreviewUrl.value;
  const originalHasPreview = editOriginalHasPreview.value;
  const previewDismissed = editLinkPreview.state.value.kind === "dismissed";
  const originalPreviewUrlRemoved = originalPreviewUrl !== null && !body.includes(originalPreviewUrl);
  const contentChanged = trimmed !== editOriginalBody.value
    || JSON.stringify(markup) !== JSON.stringify(editOriginalRich.value.markup)
    || JSON.stringify(references) !== JSON.stringify(editOriginalRich.value.references);
  const shouldResolvePreview = contentChanged
    || previewDismissed
    || originalPreviewUrlRemoved
    || editLinkPreview.state.value.kind === "ready";
  let linkPreview: Awaited<ReturnType<typeof editLinkPreview.sendPayloadFor>> | undefined;
  if (trimmed && shouldResolvePreview) {
    isSubmittingEdit.value = true;
    try {
      linkPreview = await editLinkPreview.sendPayloadFor(body);
    } finally {
      isSubmittingEdit.value = false;
    }
    if (!isEditing.value || editDraft.value !== draftAtSubmit) return;
  }
  const previewChanged = originalHasPreview
    ? previewDismissed
      || originalPreviewUrlRemoved
      || (!!linkPreview && linkPreview.preview.originalUrl !== originalPreviewUrl)
    : !!linkPreview;
  const changed = contentChanged || previewChanged;
  if (trimmed && changed) {
    emit("edit", props.message.id, body, markup, references, linkPreview);
  }
  isEditing.value = false;
  isSubmittingEdit.value = false;
  editInitialContent.value = undefined;
  editDraft.value = "";
}

function submitEditFromLink() {
  if (isSubmittingEdit.value) return;
  const doc = editEditorRef.value?.getJSON();
  if (!doc) return;
  void submitEditFromEditor(doc);
}

function emitAvatarClick() {
  emit("avatarClick", props.message.author);
}

const bubbleEl = ref<HTMLElement | null>(null);
const pickerButtonEl = ref<HTMLButtonElement | null>(null);
// Inline hover toolbar's SmilePlus popover. Desktop-only; bound to hover.
const pickerOpen = ref(false);
// Unified action sheet: touch long-press and the mobile MoreHorizontal trigger
// open the same surface so there is never more than one emoji rail on screen.
const sheetOpen = ref(false);
type SheetView = "actions" | "emoji";
const sheetView = ref<SheetView>("actions");

const desktopToolbarOwnerId = useStore($desktopToolbarOwnerId);
const desktopToolbarSuppressed = useStore($desktopToolbarSuppressed);
const desktopToolbarSuspensionEpoch = useStore($desktopToolbarSuspensionEpoch);
const ownsDesktopToolbarLock = computed(() => desktopToolbarOwnerId.value === props.message.id);
const desktopToolbarLockedByAnother = computed(() =>
  desktopToolbarOwnerId.value !== null && desktopToolbarOwnerId.value !== props.message.id,
);
const desktopToolbarVisibilityClass = computed(() => {
  if (desktopToolbarSuppressed.value) {
    return "opacity-0 motion-safe:translate-y-1 pointer-events-none z-sticky";
  }
  if (ownsDesktopToolbarLock.value || props.reactionModeSelected) {
    return "opacity-100 translate-y-0 pointer-events-auto z-floating";
  }
  return "opacity-0 motion-safe:translate-y-1 group-hover:opacity-100 group-hover:translate-y-0 focus-within:opacity-100 focus-within:translate-y-0 pointer-events-none group-hover:pointer-events-auto focus-within:pointer-events-auto z-sticky";
});
const anyOverlayOpen = computed(() => pickerOpen.value || sheetOpen.value);

function blurToolbarFocus() {
  if (typeof document === "undefined") return;
  const active = document.activeElement;
  if (!(active instanceof HTMLElement)) return;
  if (!bubbleEl.value?.contains(active)) return;
  active.blur();
}

function closePicker(blur = false) {
  if (ownsDesktopToolbarLock.value) $desktopToolbarOwnerId.set(null);
  pickerOpen.value = false;
  if (blur) blurToolbarFocus();
}

function closeSheet() {
  sheetOpen.value = false;
  sheetView.value = "actions";
}

function closeTransientMessageSurfaces() {
  pickerOpen.value = false;
  closeSheet();
  hoveredReaction.value = null;
  if (ownsDesktopToolbarLock.value) clearDesktopToolbarOwner();
  blurToolbarFocus();
}

function openSheet() {
  closePicker();
  sheetView.value = "actions";
  sheetOpen.value = true;
}

function togglePicker() {
  const next = !pickerOpen.value;
  closeSheet();
  if (next) {
    $desktopToolbarOwnerId.set(props.message.id);
    pickerOpen.value = true;
  }
  else closePicker(true);
}

function react(emoji: string) {
  emit("react", props.message.id, emoji);
  closePicker(true);
  closeSheet();
}

const reactionListFormatter = new Intl.ListFormat(undefined, { style: "long", type: "conjunction" });

function formatReactors(nicks: readonly string[]): string {
  return reactionListFormatter.format([...nicks]);
}

function reactionAriaLabel(emoji: string, nicks: readonly string[]): string {
  return `${formatReactors(nicks)} reacted with ${emoji}`;
}

function userIsReactor(nicks: readonly string[]): boolean {
  const me = props.currentUser;
  if (!me) return false;
  for (const nick of nicks) {
    if (nick === me) return true;
  }
  return false;
}

/* Warmth tier — count-proportional ambient halo on the reaction chip.
 * Replaces the "metric chip" feel with an "ambient acknowledgement"
 * read: at a glance the room shows you which posts resonated, not
 * just which ones got a tap. Thresholds are intentionally low so a
 * 3-person room still gets the warmest tier on a single shared
 * reaction. */
function reactionWarmthClass(count: number): string {
  if (count >= 5) return "chat-reaction-chip--warmth-hot";
  if (count >= 3) return "chat-reaction-chip--warmth-warm";
  if (count >= 2) return "chat-reaction-chip--warmth-tepid";
  return "";
}

// Reaction tooltip is teleported to <body> so that pane-level
// `overflow-x: hidden` (`.chat-pane-scroll`) cannot clip it. The chip stays
// in place; on hover/focus we capture the chip's bounding rect and render a
// fixed-position panel above it. Touch devices use the action sheet instead;
// keyboard reveal is gated on `:focus-visible` so a mouse click does not
// briefly flash the tooltip alongside the just-applied reaction.
const hoveredReaction = ref<{
  emoji: string;
  nicks: readonly string[];
  rect: DOMRect;
} | null>(null);

const REACTION_TOOLTIP_HALF_WIDTH = 144; // matches max-w-[18rem]
const REACTION_TOOLTIP_GAP = 8;

function showReactionFromPointer(emoji: string, nicks: readonly string[], event: PointerEvent) {
  // Touch: skip — touch devices use the action sheet for reactions and the
  // synthesized pointerenter has no matching pointerleave on tap.
  // Mouse + pen (stylus): show — both fire pointerleave reliably and
  // benefit from the hover affordance.
  if (event.pointerType !== "mouse" && event.pointerType !== "pen") return;
  const target = event.currentTarget;
  if (!(target instanceof HTMLElement)) return;
  hoveredReaction.value = { emoji, nicks, rect: target.getBoundingClientRect() };
}

function showReactionFromFocus(emoji: string, nicks: readonly string[], event: FocusEvent) {
  const target = event.currentTarget;
  if (!(target instanceof HTMLElement)) return;
  if (!target.matches(":focus-visible")) return;
  hoveredReaction.value = { emoji, nicks, rect: target.getBoundingClientRect() };
}

function hideReactionTooltip(emoji: string) {
  if (hoveredReaction.value?.emoji === emoji) hoveredReaction.value = null;
}

const reactionTooltipStyle = computed(() => {
  const hover = hoveredReaction.value;
  if (!hover) return null;
  const viewportWidth = typeof window === "undefined" ? hover.rect.right + REACTION_TOOLTIP_HALF_WIDTH : window.innerWidth;
  const wantedCenter = hover.rect.left + hover.rect.width / 2;
  const minCenter = REACTION_TOOLTIP_HALF_WIDTH + REACTION_TOOLTIP_GAP;
  const maxCenter = viewportWidth - REACTION_TOOLTIP_HALF_WIDTH - REACTION_TOOLTIP_GAP;
  const clampedCenter = Math.max(minCenter, Math.min(maxCenter, wantedCenter));
  return {
    top: `${hover.rect.top - REACTION_TOOLTIP_GAP}px`,
    left: `${clampedCenter}px`,
  };
});

function onReactionTooltipScroll() {
  hoveredReaction.value = null;
}

watch(hoveredReaction, (next) => {
  if (typeof window === "undefined") return;
  if (next) {
    window.addEventListener("scroll", onReactionTooltipScroll, true);
  }
  else {
    window.removeEventListener("scroll", onReactionTooltipScroll, true);
  }
});

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

const longPress = useLongPress({
  onLongPress: () => {
    openSheet();
  },
});

const swipe = useHorizontalSwipe({
  onSwipeLeft: () => {
    // Right-to-left drag opens (or enters) the thread for this message.
    // For root messages this jumps to their existing thread; for replies
    // it walks into the thread of the parent (matches the toolbar's
    // "open thread" affordance).
    const threadId = props.message.threadId ?? props.message.id;
    emit("openThread", threadId);
  },
  onSwipeRight: () => {
    // Left-to-right drag fills the composer reply chip targeting this
    // message — same path as the toolbar reply button.
    emit("reply", props.message);
  },
});

function onSwipePointerdown(event: PointerEvent) {
  swipe.handlers.onPointerdown(event);
  longPress.handlers.onPointerdown(event);
}
function onSwipePointermove(event: PointerEvent) {
  swipe.handlers.onPointermove(event);
  longPress.handlers.onPointermove(event);
}
function onSwipePointerup(event: PointerEvent) {
  swipe.handlers.onPointerup(event);
  longPress.handlers.onPointerup(event);
}
function onSwipePointercancel(event: PointerEvent) {
  swipe.handlers.onPointercancel(event);
  longPress.handlers.onPointercancel(event);
}
function onSwipePointerleave(event: PointerEvent) {
  swipe.handlers.onPointerleave(event);
  longPress.handlers.onPointerleave(event);
}

function onBubbleContextMenu(event: MouseEvent) {
  // Suppress iOS Safari / Android native long-press menu while the gesture is
  // being handled. Desktop right-click (pointerType 'mouse' never sets
  // isPressing) remains untouched.
  if (longPress.isPressing.value) event.preventDefault();
}

function onWindowKeydown(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  if (sheetOpen.value) closeSheet();
  else closePicker(true);
}

// Only listen globally while an overlay is actually open so a long timeline
// does not attach a handler per card. Outside-click closing for the picker is
// owned by EmojiPicker itself (its panel is teleported to <body>, so a
// bubble-relative check here would close it on every in-panel click); the
// action sheet uses a backdrop. We only handle Escape here.
watch(
  anyOverlayOpen,
  (open) => {
    if (typeof window === "undefined") return;
    if (open) window.addEventListener("keydown", onWindowKeydown);
    else window.removeEventListener("keydown", onWindowKeydown);
  },
);

watch(
  () => desktopToolbarSuspensionEpoch.value,
  () => {
    closeTransientMessageSurfaces();
  },
);

watch(
  () => desktopToolbarOwnerId.value,
  (ownerId) => {
    if (ownerId === props.message.id) return;
    if (pickerOpen.value) pickerOpen.value = false;
  },
);

onBeforeUnmount(() => {
  if (ownsDesktopToolbarLock.value) $desktopToolbarOwnerId.set(null);
});

onBeforeUnmount(() => {
  if (typeof window === "undefined") return;
  window.removeEventListener("keydown", onWindowKeydown);
  window.removeEventListener("scroll", onReactionTooltipScroll, true);
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
    :data-message-created-at="message.createdAt"
    class="chat-message-grid animate-message-in"
  >
    <div class="chat-message-avatar-cell flex items-center justify-center text-muted-foreground/60">
      <component :is="message.pinEventAction === 'unpinned' ? PinOff : Pin" class="w-4 h-4" aria-hidden="true" />
    </div>
    <div class="chat-message-body-stack">
      <p class="type-field-sm italic text-muted-foreground">
        {{ message.body }}
        <span class="type-meta type-numeric ml-2">{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
      </p>
    </div>
  </div>

  <!-- System band: bot-authored messages carrying event-intent extension
       annotations (GitHub workflow runs, deploy notifications, …).
       Rendered full-width and flat with no avatar gutter — these aren't
       chat replies from a person, they're notifications, and they
       should look like one. The bot's literal body text is suppressed
       because the structured payload already says it better. -->
  <template v-else-if="renderAsSystemBand">
    <section
      v-for="card in eventBands"
      :key="`band:${card.annotation.extensionId}:${card.annotation.annotationId}`"
      :data-message-id="message.id"
      :data-message-created-at="message.createdAt"
      class="chat-system-band animate-message-in"
      :class="[
        systemBandToneClass(card.presentation.tone),
        systemBandKindClass(card.presentation.kind),
      ]"
    >
      <div class="chat-system-band__header">
        <span class="chat-system-band__source">
          <component :is="systemBandIcon(card)" aria-hidden="true" />
          {{ card.presentation.label || message.author }}
        </span>
        <span class="chat-system-band__stamp">{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
        <span v-if="card.presentation.primaryValue" class="chat-system-band__tone-pill">
          {{ card.presentation.primaryValue }}
        </span>
      </div>
      <div class="chat-system-band__title">
        <a
          v-if="card.presentation.primaryUrl"
          :href="card.presentation.primaryUrl"
          target="_blank"
          rel="noopener noreferrer"
          class="chat-system-band__title-link"
          @click.stop
        >
          <span>{{ card.presentation.title }}</span>
          <ExternalLink aria-hidden="true" />
        </a>
        <template v-else>{{ card.presentation.title }}</template>
      </div>
      <div
        v-if="card.presentation.details.length > 0 || card.presentation.secondaryValue"
        class="chat-system-band__meta"
      >
        <span v-if="card.presentation.secondaryValue" class="chat-system-band__meta-item">
          <span class="chat-system-band__meta-value">{{ card.presentation.secondaryValue }}</span>
        </span>
        <span
          v-for="detail in card.presentation.details"
          :key="`${card.annotation.annotationId}:${detail.label}`"
          class="chat-system-band__meta-item"
        >
          <span class="chat-system-band__meta-label">{{ detail.label }}</span>
          <span
            class="chat-system-band__meta-value"
            :class="systemBandMetaValueClass(detail.label)"
            :title="detail.value"
          >{{ detail.value }}</span>
        </span>
      </div>
      <div v-if="card.annotation.actions.length > 0" class="chat-system-band__actions">
        <button
          v-for="action in card.annotation.actions"
          :key="`${card.annotation.annotationId}:${action.route}`"
          type="button"
          class="chat-extension-action-chip"
          :class="systemBandActionState(card.annotation.annotationId, action)?.state
            ? `chat-extension-action-chip--state-${systemBandActionState(card.annotation.annotationId, action)?.state}`
            : ''"
          :disabled="systemBandActionState(card.annotation.annotationId, action)?.state === 'loading' || !action.launch"
          :title="systemBandActionState(card.annotation.annotationId, action)?.detail ?? action.launch?.commandNode ?? action.label"
          @click.stop="invokeSystemBandAction(card.annotation.annotationId, action)"
        >
          {{ action.label }}
        </button>
      </div>
    </section>
  </template>

  <div
    v-else-if="message.callThread"
    :data-message-id="message.id"
    :data-message-created-at="message.createdAt"
    class="chat-message-grid animate-message-in"
  >
    <div class="chat-message-avatar-cell flex items-center justify-center text-muted-foreground/60">
      <PhoneCall class="w-4 h-4" aria-hidden="true" />
    </div>
    <div class="chat-message-body-stack">
      <button
        type="button"
        class="type-field-sm inline-flex items-center gap-1.5 text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        :disabled="!callThreadId"
        @click="openCallThreadAnchor"
      >
        <span>{{ callThreadLabel }}</span>
        <span class="type-meta type-numeric">{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
      </button>
    </div>
  </div>

  <!-- Retracted tombstone — body is gone but author/time/avatar
       stay for context. Lift the row opacity from 35 % → 55 % so
       the avatar reads as a faded-but-readable mark rather than
       a ghost, and pair the italic copy with a Trash2 glyph so
       the state has a clear iconographic signal next to the prose. -->
  <div
    v-else-if="message.isRetracted"
    :data-message-id="message.id"
    :data-message-created-at="message.createdAt"
    class="chat-message-grid opacity-55 animate-message-in"
    :class="grouped ? 'chat-message-grouped' : ''"
  >
    <div v-if="grouped" class="chat-message-avatar-cell chat-message-time-gutter">
      <span class="type-meta type-numeric text-muted-foreground/60">{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
    </div>
    <AppAvatar
      v-else
      class="chat-message-avatar-cell"
      :name="message.author"
      :src="avatarUrl"
      :presence="presence"
      :last-seen="lastSeen"
      size="message"
    />
    <div class="chat-message-body-stack">
      <div v-if="!grouped" class="chat-message-meta-row">
        <button
          type="button"
          class="type-message-author chat-message-author-button"
          :aria-label="`Open profile for ${message.author}`"
          @click.stop="emitAvatarClick"
        >{{ message.author }}</button>
        <span class="type-meta type-numeric text-muted-foreground">
          {{ formatTimelineTimeOfDay(message.createdAt) }}
        </span>
      </div>
      <p class="type-message-body italic text-muted-foreground inline-flex items-center gap-1.5">
        <Trash2 class="w-3.5 h-3.5 flex-shrink-0 opacity-70" aria-hidden="true" />
        <span>This message was deleted.</span>
      </p>
    </div>
  </div>

  <!-- Normal message -->
  <div
    v-else
    ref="bubbleEl"
    :data-message-id="message.id"
    :data-message-created-at="message.createdAt"
    :data-sheet-open="sheetOpen ? 'true' : 'false'"
    class="chat-message-grid group relative ring-1 ring-transparent transition-colors duration-150 animate-message-in chat-message-swipeable"
    :class="[
      isMentioned
        ? 'chat-message-grid--mention'
        : isForumTopic
          ? 'chat-message-grid--forum shadow-sm'
          : message.threadId
            ? 'chat-message-grid--thread'
            : '',
      message.deliveryStatus === 'sending' || message.deliveryStatus === 'queued' ? 'opacity-50' : '',
      grouped ? 'chat-message-grouped' : '',
      reactionModeSelected ? 'chat-message-grid--reaction-selected' : '',
      swipe.isSwiping.value ? 'chat-message-swipe-active' : '',
      swipe.isArmed.value && swipe.direction.value === -1 ? 'chat-message-swipe-armed-thread' : '',
      swipe.isArmed.value && swipe.direction.value === 1 ? 'chat-message-swipe-armed-reply' : '',
    ]"
    :style="{
      '--chat-swipe-x': swipe.translateX.value + 'px',
      transform: `translateX(${swipe.translateX.value}px)`,
    }"
    @pointerdown="onSwipePointerdown"
    @pointermove="onSwipePointermove"
    @pointerup="onSwipePointerup"
    @pointercancel="onSwipePointercancel"
    @pointerleave="onSwipePointerleave"
    @contextmenu="onBubbleContextMenu"
  >
    <span class="chat-message-swipe-hint chat-message-swipe-hint--reply" aria-hidden="true" />
    <span class="chat-message-swipe-hint chat-message-swipe-hint--thread" aria-hidden="true" />
    <div v-if="grouped" class="chat-message-avatar-cell chat-message-time-gutter" aria-hidden="true">
      <span class="type-meta type-numeric text-muted-foreground/60">{{ formatTimelineTimeOfDay(message.createdAt) }}</span>
    </div>
    <button
      v-else
      class="chat-message-avatar-cell rounded-lg"
      type="button"
      :aria-label="`Open profile for ${message.author}`"
      @click.stop="emitAvatarClick"
    >
      <AppAvatar :name="message.author" :src="avatarUrl" :presence="presence" :last-seen="lastSeen" size="message" />
    </button>
    <!-- Thread rail glyph — a small "messages-stack" icon centred in the
         avatar column, vertically aligned with the in-body chip's
         avatar row. Structural marker that this row anchors a thread;
         the rich summary (who replied, how many, how recently) lives
         in the in-body chip. Position-absolute relative to the row's
         grid; bottom-anchored so its centre line matches the chip
         avatars' centre line — see .chat-thread-rail-glyph in
         messages.css for the offset derivation. -->
    <div
      v-if="showThreadChip"
      class="chat-thread-rail-glyph"
      aria-hidden="true"
    >
      <MessagesSquare class="chat-thread-rail-glyph__icon" />
    </div>
    <div class="chat-message-body-stack">
      <div v-if="!grouped" class="chat-message-meta-row">
        <button
          type="button"
          class="type-message-author chat-message-author-button"
          :aria-label="`Open profile for ${message.author}`"
          @click.stop="emitAvatarClick"
        >{{ message.author }}</button>
        <span
          v-if="authorBadge"
          class="chat-hat-tag"
          :class="authorBadge.colorClass"
          :title="authorBadgeTooltip"
        >{{ authorBadge.label }}</span>
        <span class="type-meta type-numeric text-muted-foreground/60">
          {{ formatTimelineTimeOfDay(message.createdAt) }}
        </span>
        <span
          v-if="isPinned"
          class="inline-flex items-center text-muted-foreground/70"
          title="Pinned in this channel"
          aria-label="Pinned in this channel"
        >
          <Pin class="w-3 h-3" aria-hidden="true" />
        </span>
        <span v-if="message.isEdited" class="type-meta text-muted-foreground/50">(edited)</span>
        <span
          v-if="message.isSelf && deliveryStatusLabel"
          class="type-meta inline-flex items-center gap-1"
          :class="deliveryStatusClass"
        >
          <component
            v-if="deliveryStatusIcon"
            :is="deliveryStatusIcon"
            class="w-3 h-3"
            :class="message.deliveryStatus === 'sending' ? 'motion-safe:animate-spin' : ''"
            aria-hidden="true"
          />
          {{ deliveryStatusLabel }}
        </span>
        <span
          v-if="message.isSelf && message.readBy && message.readBy.length > 0"
          class="type-meta text-muted-foreground/50"
          :title="message.readBy.join(', ')"
        >
          Read by {{ message.readBy.length }}
        </span>
      </div>

    <div
      v-if="isForumTopic && forumThreadLabel"
      class="chat-forum-topic-card chat-message-fill"
    >
      <div class="type-section-label text-primary/75">
        Topic
      </div>
      <h3 class="type-card-title text-foreground">
        {{ forumThreadLabel }}
      </h3>
    </div>
    <div
      v-else-if="isForumReply && forumThreadLabel"
      class="chat-forum-reply-chip chat-message-fill type-caption"
    >
      <CornerDownRight class="w-3 h-3 flex-shrink-0 text-primary/70" />
      <span class="truncate">In {{ forumThreadLabel }}</span>
    </div>
    <!-- Reply preview chip. Clicking scrolls to the parent message; if the
         preview is available we also expand it inline so users still see the
         full quoted text even when the parent has scrolled off-screen or
         hasn't loaded from history yet.
         Visual language matches iter-42's composer reply chip: a 3 px
         primary-tinted left rail + italic preview text so "you are
         replying" and "this is a reply" speak one dialect. -->
    <div v-if="message.replyTo && !hideReplyChip" class="chat-message-fill">
      <button
        type="button"
        class="type-caption flex min-h-7 max-w-full items-center gap-1.5 rounded-lg border-l-[3px] border-l-primary/55 bg-muted/35 px-2 text-left text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
        :aria-expanded="replyChipExpanded"
        :title="message.replyTo.preview ? 'Show full quoted message and jump to it' : 'Jump to replied message'"
        @click="onReplyChipClick"
      >
        <CornerDownRight class="w-3 h-3 flex-shrink-0 text-primary/70" />
        <span class="type-emphasis text-primary/80">@{{ replyAuthorName }}</span>
        <span
          v-if="message.replyTo.preview"
          :class="['flex-1 min-w-0 italic opacity-75', replyChipExpanded ? 'whitespace-pre-wrap break-words' : 'truncate']"
        >{{ message.replyTo.preview }}</span>
        <span v-else class="type-mono opacity-60">{{ message.replyTo.id.slice(0, 8) }}</span>
      </button>
    </div>

    <!-- Edit mode -->
    <div v-if="isEditing" class="chat-message-fill flex min-w-0 items-start gap-1.5">
      <div class="flex min-w-0 flex-1 flex-col gap-1.5">
        <ChatEditor
          :ref="setEditEditorRef"
          compact
          :initial-content="editInitialContent"
          placeholder="Edit message…"
          @send="submitEditFromEditor"
          @update="updateEditDraft"
          @cancel="cancelEdit"
        />
        <div
          v-if="editLinkPreview.showCard.value"
          class="flex min-w-0 items-center gap-2 rounded-md border border-border bg-muted/45 px-2 py-1.5"
          :aria-busy="editLinkPreview.state.value.kind === 'loading'"
        >
          <Loader2
            v-if="editLinkPreview.state.value.kind === 'loading'"
            class="h-4 w-4 shrink-0 animate-spin text-primary"
            aria-hidden="true"
          />
          <div class="min-w-0 flex-1">
            <div class="type-emphasis truncate text-foreground">{{ editLinkPreview.title.value }}</div>
            <div class="type-caption truncate text-muted-foreground">{{ editLinkPreview.description.value }}</div>
          </div>
          <button
            v-if="editLinkPreview.canDismiss.value"
            type="button"
            class="chat-composer-input-action h-7 w-7 shrink-0 flex items-center justify-center text-muted-foreground transition-colors hover:bg-background/70 hover:text-foreground"
            aria-label="Remove preview"
            @click="editLinkPreview.dismiss"
          >
            <X class="h-4 w-4" aria-hidden="true" />
          </button>
        </div>
        <p class="type-caption text-muted-foreground/70">
          escape to
          <button
            type="button"
            class="type-emphasis text-primary/85 transition-colors hover:text-primary hover:underline"
            @click="cancelEdit"
          >
            cancel
          </button>
          <span class="mx-1 text-muted-foreground/35">•</span>
          <button
            type="button"
            class="type-emphasis text-primary/85 transition-colors hover:text-primary hover:underline"
            @click="submitEditFromLink"
          >
            enter
          </button>
          to save
        </p>
      </div>
      <EditorBubbleToolbar v-if="editTiptapEditor" :editor="editTiptapEditor" />
    </div>

    <MessageBody
      v-else
      :message="message"
      :invoke-extension-action="invokeExtensionAction"
    />

    <!-- Thread replies affordance. Visible in the main channel feed on roots
         that have replies; the thread panel hides it via hideThreadChip since
         the panel already shows children. The chip carries a row of
         participant avatars (current user excluded by the caller) + reply
         count, with the recency timestamp right-aligned so the eye can
         triage threads at a glance — who's been talking, how many turns,
         how recently — without opening the panel. The structural "this is
         a thread" marker lives in the avatar gutter as the rail glyph. -->
    <button
      v-if="showThreadChip"
      type="button"
      class="chat-thread-chip type-caption flex w-full items-center rounded-md py-0.5 text-primary/85 transition-colors hover:text-primary"
      :title="`Open thread (${threadReplyCount} ${threadReplyCount === 1 ? 'reply' : 'replies'})`"
      @click="openThreadFromChip"
    >
      <span
        v-if="visibleThreadParticipants.length > 0"
        class="chat-thread-chip__avatars"
        aria-hidden="true"
      >
        <span
          v-for="participant in visibleThreadParticipants"
          :key="`thread-chip-avatar:${message.id}:${participant.nick}`"
          class="chat-thread-chip__avatar-wrap"
        >
          <AppAvatar
            :name="participant.nick"
            :src="participant.avatarUrl ?? null"
            :presence="participant.presence"
            size="xs"
          />
        </span>
        <span
          v-if="threadParticipantOverflow > 0"
          class="chat-thread-chip__overflow"
        >+{{ threadParticipantOverflow }}</span>
      </span>
      <span class="chat-thread-chip__count min-w-0 truncate">{{ threadReplyCount }} {{ threadReplyCount === 1 ? "reply" : "replies" }}</span>
      <span v-if="threadChipRecency" class="chat-thread-chip__recency">{{ threadChipRecency }}</span>
    </button>

    <!-- Existing reactions (inline, always visible when present) -->
    <div v-if="message.reactions && Object.keys(message.reactions).length > 0" class="chat-message-reactions flex flex-wrap gap-1">
      <button
        v-for="(nicks, emoji) in message.reactions"
        :key="emoji"
        type="button"
        class="chat-reaction-chip type-caption inline-flex h-7 items-center gap-1 px-2 rounded-lg"
        :class="[
          userIsReactor(nicks) ? 'chat-reaction-chip--self' : '',
          reactionWarmthClass(nicks.length),
        ]"
        :aria-label="reactionAriaLabel(emoji, nicks)"
        :aria-pressed="userIsReactor(nicks)"
        @click="emit('react', message.id, emoji)"
        @pointerenter="(event) => showReactionFromPointer(emoji, nicks, event)"
        @pointerleave="() => hideReactionTooltip(emoji)"
        @focusin="(event) => showReactionFromFocus(emoji, nicks, event)"
        @focusout="() => hideReactionTooltip(emoji)"
      >
        <span class="chat-reaction-chip__glyph">{{ emoji }}</span>
        <span class="type-meta type-numeric chat-reaction-chip__count">{{ nicks.length }}</span>
      </button>
    </div>

    <!-- Floating action toolbar — desktop-only hover/focus affordance. On
         touch devices (where hover never fires) long-press opens the action
         sheet instead, so this toolbar stays hidden and we never show two
         emoji rails at once. -->
    <div
      v-if="!isEditing && !desktopToolbarLockedByAnother"
      :class="[
        'chat-hover-action-toolbar absolute -top-4 right-3 flex items-center gap-1 transition-[opacity,transform] duration-150 ease-out bg-card/95 backdrop-blur border border-border rounded-lg shadow-[0_10px_28px_-12px_var(--glow-strong),0_4px_12px_-4px_color-mix(in_oklab,var(--foreground)_20%,transparent)] p-1 [@media(pointer:coarse)]:hidden',
        desktopToolbarVisibilityClass,
        reactionModeSelected ? 'chat-hover-action-toolbar--reaction-mode' : '',
      ]"
      :role="reactionModeSelected ? 'status' : undefined"
      :aria-live="reactionModeSelected ? 'polite' : undefined"
    >
      <button
        v-for="(e, index) in quickEmojis"
        :key="e"
        type="button"
        class="chat-hover-action-toolbar-btn type-emoji-button relative h-8 w-8 flex items-center justify-center rounded-md hover:bg-muted motion-safe:hover:scale-110"
        :title="`React with ${e}`"
        :aria-label="`React to message with ${e}`"
        @click="react(e)"
      >
        <span
          v-if="reactionModeSelected"
          class="chat-reaction-mode-keycap type-meta type-numeric"
          aria-hidden="true"
        >{{ index + 1 }}</span>
        {{ e }}
      </button>
      <div class="relative">
        <button
          ref="pickerButtonEl"
          type="button"
          class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
          :class="pickerOpen ? 'bg-muted text-foreground' : ''"
          title="Add reaction"
          aria-label="Add reaction"
          :aria-expanded="pickerOpen"
          aria-haspopup="dialog"
          @click="togglePicker"
        >
          <SmilePlus class="w-4 h-4" aria-hidden="true" />
        </button>
        <EmojiPicker
          :open="pickerOpen"
          :anchor-el="pickerButtonEl"
          @select="react"
          @close="closePicker(true)"
        />
      </div>
      <button
        v-if="canReplyToMessage"
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        title="Reply"
        aria-label="Reply to message"
        @click="startReplyFromMenu"
      >
        <Reply class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        :title="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
        :aria-label="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
        @click="startReplyInThreadFromMenu"
      >
        <MessageSquare class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        v-if="canPinMessages && message.id"
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        :title="isPinned ? 'Unpin from channel' : 'Pin to channel'"
        :aria-label="isPinned ? 'Unpin from channel' : 'Pin to channel'"
        @click="togglePinFromMenu"
      >
        <component :is="isPinned ? PinOff : Pin" class="w-4 h-4" aria-hidden="true" />
      </button>
      <template v-if="message.isSelf">
        <div class="w-px h-5 bg-border mx-0.5" />
        <button
          type="button"
          class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
          title="Edit message"
          aria-label="Edit message"
          @click="startEditFromMenu"
        >
          <Pencil class="w-4 h-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10"
          title="Delete message"
          aria-label="Delete message"
          @click="retractFromMenu"
        >
          <Trash2 class="w-4 h-4" aria-hidden="true" />
        </button>
      </template>
    </div>
    </div>

    <!-- Action-sheet trigger. Touch-only; desktop already has the hover toolbar.
         Lives at a quiet 25 % opacity on a relaxed timeline so a column of
         repeating ••• buttons doesn't out-shout the messages themselves.
         Lifts to full opacity while the row is focused-within (e.g. mid
         long-press) or while its action sheet is open. -->
    <button
      v-if="!isEditing"
      type="button"
      class="chat-message-action-trigger z-sticky absolute top-1 right-1 hidden [@media(pointer:coarse)]:flex h-11 w-11 items-center justify-center rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted active:bg-muted transition-all duration-150"
      :class="sheetOpen ? 'opacity-100' : ''"
      title="Message actions"
      aria-label="Message actions"
      :aria-expanded="sheetOpen"
      aria-haspopup="dialog"
      @click="openSheet"
    >
      <MoreHorizontal class="w-5 h-5" aria-hidden="true" />
    </button>
  </div>

  <!-- Unified action sheet: opened by touch long-press or the MoreHorizontal
       trigger. Teleported so it escapes overflow-hidden
       ancestors; anchored at the bottom on mobile for large touch targets
       and centred when opened from a wider touch viewport. -->
  <Teleport to="body">
    <div
      v-if="sheetOpen"
      class="z-modal fixed inset-0 flex items-end sm:items-center justify-center animate-fade-in"
      role="presentation"
    >
      <div class="absolute inset-0 bg-background/60 backdrop-blur-sm" @click="closeSheet" />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Message actions"
        class="chat-action-sheet-stack relative w-full sm:max-w-sm glass-panel border border-border rounded-t-lg sm:rounded-lg shadow-2xl animate-slide-up p-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]"
        @pointerdown.stop
      >
        <div class="chat-action-sheet-handle sm:hidden">
          <div class="h-1 w-10 rounded-full bg-muted-foreground/30" />
        </div>

        <template v-if="sheetView === 'actions'">
          <div class="chat-action-sheet-reactions">
            <button
              v-for="e in quickEmojis"
              :key="`sheet-${e}`"
              type="button"
              class="type-emoji-sheet h-12 flex items-center justify-center rounded-lg hover:bg-muted active:bg-muted transition-colors"
              :aria-label="`React with ${e}`"
              @click="react(e)"
            >{{ e }}</button>
            <button
              type="button"
              class="h-12 flex items-center justify-center rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted active:bg-muted transition-colors"
              aria-label="More reactions"
              @click="sheetView = 'emoji'"
            >
              <SmilePlus class="w-5 h-5" aria-hidden="true" />
            </button>
          </div>

          <button
            v-if="canReplyToMessage"
            type="button"
            class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
            @click="startReplyFromMenu"
          >
            <Reply class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>Reply</span>
          </button>
          <button
            type="button"
            class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
            @click="startReplyInThreadFromMenu"
          >
            <MessageSquare class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>{{ (threadReplyCount ?? 0) > 0 ? "Open thread" : "Reply in thread" }}</span>
          </button>
          <button
            v-if="canPinMessages && message.id"
            type="button"
            class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
            @click="togglePinFromMenu"
          >
            <component :is="isPinned ? PinOff : Pin" class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>{{ isPinned ? "Unpin from channel" : "Pin to channel" }}</span>
          </button>
          <template v-if="message.isSelf">
            <button
              type="button"
              class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
              @click="startEditFromMenu"
            >
              <Pencil class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
              <span>Edit</span>
            </button>
            <button
              type="button"
              class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg text-destructive hover:bg-destructive/10 active:bg-destructive/10 transition-colors text-left"
              @click="retractFromMenu"
            >
              <Trash2 class="w-5 h-5" aria-hidden="true" />
              <span>Delete</span>
            </button>
          </template>
          <button
            type="button"
            class="type-field sm:hidden w-full h-12 rounded-lg text-muted-foreground hover:bg-muted active:bg-muted transition-colors"
            @click="closeSheet"
          >Cancel</button>
        </template>

        <template v-else>
          <EmojiPicker
            :open="true"
            variant="sheet"
            @select="react"
            @close="sheetView = 'actions'"
          />
        </template>
      </div>
    </div>
  </Teleport>

  <!-- Reaction tooltip: teleported so it escapes `.chat-pane-scroll`'s
       `overflow-x: hidden`. Anchored above the hovered chip via the captured
       rect. -->
  <Teleport to="body">
    <div
      v-if="hoveredReaction && reactionTooltipStyle"
      aria-hidden="true"
      class="chat-reaction-tooltip pointer-events-none fixed z-popover flex max-w-[18rem] -translate-x-1/2 -translate-y-full flex-col items-center gap-0.5 rounded-md border border-border bg-popover px-2 py-1.5 text-popover-foreground shadow-md"
      :style="reactionTooltipStyle"
    >
      <span class="text-lg leading-none" aria-hidden="true">{{ hoveredReaction.emoji }}</span>
      <span class="type-meta text-center text-muted-foreground">
        {{ formatReactors(hoveredReaction.nicks) }}
      </span>
    </div>
  </Teleport>
</template>
