<script setup lang="ts">
import { ref, computed } from "vue";
import {
  AlertCircle,
  Clock,
  Loader2,
  MoreHorizontal,
  CornerDownRight,
  MessagesSquare,
  PhoneCall,
  Pin,
  PinOff,
  Trash2,
} from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import CallAnchorCard from "@/components/calls/CallAnchorCard.vue";
import MessageBody from "@/components/chat/MessageBody.vue";
import MessageActionSheet from "@/components/chat/MessageActionSheet.vue";
import MessageEditForm from "@/components/chat/MessageEditForm.vue";
import MessageHoverToolbar from "@/components/chat/MessageHoverToolbar.vue";
import MessageReactionChips from "@/components/chat/MessageReactionChips.vue";
import MessageSystemBand from "@/components/chat/MessageSystemBand.vue";
import type {
  TimelineMessage,
  ExtensionAnnotationAction,
  MarkupSpan,
  MessageReference,
} from "@/lib/chat-ui";
import { messageMentionsBareJid } from "@/lib/mentions";
import { jidLocalpart } from "@/lib/xmpp/jid";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { OccupantAuthority, OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatTimelineTimeOfDay } from "@/channels/timeline";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import { callThreadAnchorLabel, callThreadAnchorThreadId, useCallAnchorCardState } from "@/lib/call-thread-anchor";
import { resolveThreadActionTarget } from "@/lib/thread-action-target";
import type { CallMedia } from "@/lib/calls/types";
import { authorBadge as authorBadgeFor, authorBadgeTooltip as authorBadgeTooltipFor } from "./message-card-badges";
import { eventBandsFor, rendersAsSystemBand } from "./message-system-band";
import { formatThreadRecency } from "./message-thread-recency";
import { useMessageActionSurfaces } from "./composables/use-message-action-surfaces";
import { useMessageGestures } from "./composables/use-message-gestures";

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
  avatarClick: [author: string];
  openThread: [threadId: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  pin: [messageId: string];
  unpin: [messageId: string];
}>();

const authorBadge = computed(() => authorBadgeFor(props.authority, props.hats));
const authorBadgeTooltip = computed(() => authorBadgeTooltipFor(props.authority, props.hats));

const eventBands = computed(() => eventBandsFor(props.message.extensionAnnotations));
const renderAsSystemBand = computed(() => rendersAsSystemBand(eventBands.value));

const replyAuthorName = computed(() => {
  const author = props.message.replyTo?.author;
  if (!author) return "";
  return author.includes("/") ? author.split("/").pop() ?? author : jidLocalpart(author);
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
  emit("avatarClick", props.message.author);
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
    <MessageSystemBand
      v-for="card in eventBands"
      :key="`band:${card.annotation.extensionId}:${card.annotation.annotationId}`"
      :card="card"
      :message-id="message.id"
      :message-created-at="message.createdAt"
      :message-author="message.author"
      :invoke-extension-action="invokeExtensionAction"
    />
  </template>

  <div
    v-else-if="message.callThread && !hideCallAnchorCard"
    :data-message-id="message.id"
    :data-message-created-at="message.createdAt"
    class="chat-message-grid relative animate-message-in"
  >
    <div class="chat-message-avatar-cell flex items-center justify-center text-muted-foreground/60">
      <PhoneCall class="w-4 h-4" aria-hidden="true" />
    </div>
    <div class="chat-message-body-stack">
      <CallAnchorCard
        v-if="callAnchorCardState"
        :state="callAnchorCardState"
        @join="joinCallThreadAnchor"
        @open-thread="openCallThreadAnchor"
      />
      <button
        v-else
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
      :title="`Open thread (${threadReplyCountValue} ${threadReplyCountValue === 1 ? 'reply' : 'replies'})`"
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
      <span class="chat-thread-chip__count min-w-0 truncate">{{ threadReplyCountValue }} {{ threadReplyCountValue === 1 ? "reply" : "replies" }}</span>
      <span v-if="threadChipRecency" class="chat-thread-chip__recency">{{ threadChipRecency }}</span>
    </button>

    <MessageReactionChips
      v-if="message.reactions && Object.keys(message.reactions).length > 0"
      :reactions="message.reactions"
      :current-user="currentUser"
      @react="(emoji) => emit('react', message.id, emoji)"
    />

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
