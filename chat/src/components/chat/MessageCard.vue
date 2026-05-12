<script setup lang="ts">
import { useStore } from "@nanostores/vue";
import { ref, computed, nextTick, onBeforeUnmount, watch } from "vue";
import {
  MoreHorizontal,
  Pencil,
  Reply,
  SmilePlus,
  Trash2,
  CornerDownRight,
  MessageSquare,
  Pin,
  PinOff,
} from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import MessageBody from "@/components/chat/MessageBody.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import ImageLightbox from "@/components/ui/ImageLightbox.vue";
import {
  type TimelineMessage,
  type ExtensionAnnotationAction,
  type MarkupSpan,
  type MessageReference,
} from "@/lib/chat-ui";
import { messageMentionsBareJid } from "@/lib/mentions";
import { richMessageToTiptap, tiptapToRichMessage } from "@/lib/rich-message";
import type { OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatTimelineTimeOfDay } from "@/channels/timeline";
import { useLongPress } from "@/ui/gestures/long-press";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import { $desktopToolbarOwnerId } from "@/stores/message-toolbar";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";

const HAT_LABELS: Record<string, string> = {
  "urn:xmpp:hats:owner": "OWNER",
  "urn:xmpp:hats:admin": "ADMIN",
  "urn:xmpp:hats:moderator": "MOD",
  "urn:xmpp:hats:bot": "BOT",
  "urn:xmpp:hats:verified": "VERIFIED",
};

const HAT_COLORS: Record<string, string> = {
  "urn:xmpp:hats:owner": "bg-warning/10 text-warning",
  "urn:xmpp:hats:admin": "bg-primary/10 text-primary",
  "urn:xmpp:hats:moderator": "bg-primary/10 text-primary",
  "urn:xmpp:hats:bot": "bg-success/10 text-success",
  "urn:xmpp:hats:verified": "bg-primary/10 text-primary",
};

// Higher rank wins when an author holds multiple hats. Owner subsumes
// moderator (an owner can already moderate), so per-message we render only
// the senior hat to keep the meta row breathable on mobile. The full hat
// list is still passed through so the avatar/profile surface can show it.
const HAT_RANK: Record<string, number> = {
  "urn:xmpp:hats:owner": 4,
  "urn:xmpp:hats:admin": 3,
  "urn:xmpp:hats:moderator": 2,
  "urn:xmpp:hats:verified": 1,
  "urn:xmpp:hats:bot": 0,
};

const props = defineProps<{
  message: TimelineMessage;
  currentUser?: string;
  currentUserJid?: string;
  hats: OccupantHat[];
  avatarUrl?: string | null;
  presence?: OccupantPresence;
  lastSeen?: number;
  authorJid?: string;
  threadReplyCount?: number;
  hideThreadChip?: boolean;
  grouped?: boolean;
  reactionModeSelected?: boolean;
  invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>;
  /** #414: this message is pinned in its host room. */
  isPinned?: boolean;
  /** #414: current user is room Owner or Admin (controls pin/unpin
   * action-sheet entry visibility). */
  canPinMessages?: boolean;
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]];
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
const seniorHat = computed<OccupantHat | null>(() => {
  if (!props.hats || props.hats.length === 0) return null;
  let best: OccupantHat | null = null;
  let bestRank = -Infinity;
  for (const hat of props.hats) {
    const rank = HAT_RANK[hat.uri] ?? 0;
    if (rank > bestRank || (rank === bestRank && best && hat.uri < best.uri)) {
      best = hat;
      bestRank = rank;
    }
  }
  return best;
});

// Pre-resolved lightbox images delivered by MessageBody's onImageClick.
// MessageBody builds this list from its own useMessageAttachments state,
// which holds the decrypted blob URLs and uses the canonical imageAttachments
// predicate — so neither encrypted attachment URLs nor wrong-predicate index
// mismatches can occur here.
const lightboxImages = ref<Array<{ url: string; name?: string; width?: number; height?: number }>>([]);
const lightboxOpen = ref(false);
const lightboxIndex = ref(0);

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

function startEdit() {
  const content = richMessageToTiptap({
    body: props.message.body,
    markup: props.message.markup,
    references: props.message.references,
  });
  editInitialContent.value = content;
  isEditing.value = true;
  void nextTick(() => editEditorRef.value?.focus());
}

function cancelEdit() {
  isEditing.value = false;
  editInitialContent.value = undefined;
}

function submitEditFromEditor(doc: JSONContent) {
  const { body, markup, references } = tiptapToRichMessage(doc);
  const trimmed = body.trim();
  const changed = trimmed !== editOriginalBody.value
    || JSON.stringify(markup) !== JSON.stringify(editOriginalRich.value.markup)
    || JSON.stringify(references) !== JSON.stringify(editOriginalRich.value.references);
  if (trimmed && changed) {
    emit("edit", props.message.id, body, markup, references);
  }
  isEditing.value = false;
  editInitialContent.value = undefined;
}

function submitEditFromLink() {
  const doc = editEditorRef.value?.getJSON();
  if (!doc) return;
  submitEditFromEditor(doc);
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
const ownsDesktopToolbarLock = computed(() => desktopToolbarOwnerId.value === props.message.id);
const desktopToolbarLockedByAnother = computed(() =>
  desktopToolbarOwnerId.value !== null && desktopToolbarOwnerId.value !== props.message.id,
);
const desktopToolbarVisibilityClass = computed(() => (
  ownsDesktopToolbarLock.value || props.reactionModeSelected
    ? "opacity-100 pointer-events-auto z-floating"
    : "opacity-0 group-hover:opacity-100 focus-within:opacity-100 pointer-events-none group-hover:pointer-events-auto focus-within:pointer-events-auto z-sticky"
));
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

  <!-- Retracted tombstone -->
  <div
    v-else-if="message.isRetracted"
    :data-message-id="message.id"
    :data-message-created-at="message.createdAt"
    class="chat-message-grid opacity-35 animate-message-in"
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
        <span class="type-message-author">{{ message.author }}</span>
        <span class="type-meta type-numeric text-muted-foreground">
          {{ formatTimelineTimeOfDay(message.createdAt) }}
        </span>
      </div>
      <p class="type-message-body italic text-muted-foreground">This message was deleted.</p>
    </div>
  </div>

  <!-- Normal message -->
  <div
    v-else
    ref="bubbleEl"
    :data-message-id="message.id"
    :data-message-created-at="message.createdAt"
    :data-sheet-open="sheetOpen ? 'true' : 'false'"
    class="chat-message-grid group relative ring-1 ring-transparent transition-colors duration-150 animate-message-in"
    :class="[
      isMentioned
        ? 'chat-message-grid--mention'
        : isForumTopic
          ? 'chat-message-grid--forum shadow-sm'
          : message.threadId
            ? 'chat-message-grid--thread'
            : '',
      message.deliveryStatus === 'sending' || message.deliveryStatus === 'queued' ? 'opacity-50' : '',
      longPress.isPressing.value ? 'no-callout' : '',
      grouped ? 'chat-message-grouped' : '',
      reactionModeSelected ? 'chat-message-grid--reaction-selected' : '',
    ]"
    @pointerdown="longPress.handlers.onPointerdown"
    @pointermove="longPress.handlers.onPointermove"
    @pointerup="longPress.handlers.onPointerup"
    @pointercancel="longPress.handlers.onPointercancel"
    @pointerleave="longPress.handlers.onPointerleave"
    @contextmenu="onBubbleContextMenu"
  >
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
    <div class="chat-message-body-stack">
      <div v-if="!grouped" class="chat-message-meta-row">
        <span class="type-message-author">{{ message.author }}</span>
        <span
          v-if="seniorHat"
          class="type-badge inline-block rounded-md px-1.5 py-px"
          :class="HAT_COLORS[seniorHat.uri] ?? 'bg-muted text-muted-foreground'"
          :title="hats.length > 1 ? hats.map(h => HAT_LABELS[h.uri] ?? h.title).join(' · ') : seniorHat.title"
        >{{ HAT_LABELS[seniorHat.uri] ?? seniorHat.title }}</span>
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
          class="type-meta"
          :class="deliveryStatusClass"
        >
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
         hasn't loaded from history yet. -->
    <div v-if="message.replyTo" class="chat-message-fill">
      <button
        type="button"
        class="type-caption flex min-h-7 max-w-full items-center gap-1.5 rounded-lg bg-muted/35 px-2 text-left text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
        :aria-expanded="replyChipExpanded"
        :title="message.replyTo.preview ? 'Show full quoted message and jump to it' : 'Jump to replied message'"
        @click="onReplyChipClick"
      >
        <CornerDownRight class="w-3 h-3 flex-shrink-0" />
        <span class="type-emphasis text-primary/80">@{{ replyAuthorName }}</span>
        <span
          v-if="message.replyTo.preview"
          :class="['flex-1 min-w-0 opacity-70', replyChipExpanded ? 'whitespace-pre-wrap break-words' : 'truncate']"
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
          @cancel="cancelEdit"
        />
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
      :on-image-click="(images, index) => { lightboxImages.value = images; lightboxIndex.value = index; lightboxOpen.value = true; }"
    />

    <ImageLightbox
      v-model:open="lightboxOpen"
      v-model:index="lightboxIndex"
      :images="lightboxImages"
    />

    <!-- Thread replies affordance. Visible in the main channel feed on roots
         that have replies; the thread panel hides it via hideThreadChip since
         the panel already shows children. -->
    <button
      v-if="showThreadChip"
      type="button"
      class="chat-thread-chip type-caption type-emphasis inline-flex h-7 items-center gap-1.5 rounded-md bg-primary/10 px-2 text-primary transition-colors hover:bg-primary/20"
      :title="`Open thread (${threadReplyCount} ${threadReplyCount === 1 ? 'reply' : 'replies'})`"
      @click="openThreadFromChip"
    >
      <MessageSquare class="w-3 h-3 flex-shrink-0" />
      <span class="min-w-0 truncate">{{ threadReplyCount }} {{ threadReplyCount === 1 ? "reply" : "replies" }}</span>
    </button>

    <!-- Existing reactions (inline, always visible when present) -->
    <div v-if="message.reactions && Object.keys(message.reactions).length > 0" class="chat-message-reactions flex flex-wrap gap-1">
      <button
        v-for="(nicks, emoji) in message.reactions"
        :key="emoji"
        type="button"
        class="chat-reaction-chip group/reaction relative type-caption inline-flex h-7 items-center gap-1 px-2 rounded-lg bg-muted/60 hover:bg-muted transition-all duration-200"
        :aria-label="reactionAriaLabel(emoji, nicks)"
        @click="emit('react', message.id, emoji)"
      >
        <span>{{ emoji }}</span>
        <span class="type-meta type-numeric text-muted-foreground">{{ nicks.length }}</span>
        <span
          aria-hidden="true"
          class="chat-reaction-tooltip pointer-events-none absolute bottom-full left-1/2 z-popover mb-1 hidden -translate-x-1/2 max-w-xs rounded-md border border-border bg-popover px-2 py-1.5 text-popover-foreground shadow-md group-hover/reaction:flex group-focus-visible/reaction:flex flex-col items-center gap-0.5"
        >
          <span class="text-lg leading-none" aria-hidden="true">{{ emoji }}</span>
          <span class="type-meta text-center text-muted-foreground">
            {{ formatReactors(nicks) }}
          </span>
        </span>
      </button>
    </div>

    <!-- Floating action toolbar — desktop-only hover/focus affordance. On
         touch devices (where hover never fires) long-press opens the action
         sheet instead, so this toolbar stays hidden and we never show two
         emoji rails at once. -->
    <div
      v-if="!isEditing && !desktopToolbarLockedByAnother"
      :class="[
        'chat-hover-action-toolbar absolute -top-4 right-3 flex items-center gap-1 transition-opacity duration-150 bg-card/95 backdrop-blur border border-border rounded-lg shadow-lg p-1 [@media(pointer:coarse)]:hidden',
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
        class="type-emoji-button relative h-8 w-8 flex items-center justify-center rounded-md hover:bg-muted hover:scale-105 transition-all duration-150"
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
          class="h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
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
        class="h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
        title="Reply"
        aria-label="Reply to message"
        @click="startReplyFromMenu"
      >
        <Reply class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
        :title="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
        :aria-label="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
        @click="startReplyInThreadFromMenu"
      >
        <MessageSquare class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        v-if="canPinMessages && message.id"
        type="button"
        class="h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
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
          class="h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
          title="Edit message"
          aria-label="Edit message"
          @click="startEditFromMenu"
        >
          <Pencil class="w-4 h-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-all duration-150"
          title="Delete message"
          aria-label="Delete message"
          @click="retractFromMenu"
        >
          <Trash2 class="w-4 h-4" aria-hidden="true" />
        </button>
      </template>
    </div>
    </div>

    <!-- Action-sheet trigger. Touch-only; desktop already has the hover toolbar. -->
    <button
      v-if="!isEditing"
      type="button"
      class="z-sticky absolute top-1 right-1 hidden h-8 w-8 [@media(pointer:coarse)]:flex [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11 items-center justify-center rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted opacity-70 transition-all duration-150"
      title="Message actions"
      aria-label="Message actions"
      :aria-expanded="sheetOpen"
      aria-haspopup="dialog"
      @click="openSheet"
    >
      <MoreHorizontal class="w-4 h-4 [@media(pointer:coarse)]:w-5 [@media(pointer:coarse)]:h-5" aria-hidden="true" />
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
</template>
