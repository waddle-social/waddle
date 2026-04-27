<script setup lang="ts">
import { useStore } from "@nanostores/vue";
import { ref, computed, nextTick, onBeforeUnmount, watch } from "vue";
import {
  MoreHorizontal,
  Pencil,
  Reply,
  SmilePlus,
  Trash2,
  FileDown,
  CornerDownRight,
  MessageSquare,
  Lock,
  Github,
  GitPullRequest,
  CircleDot,
  Bot,
  ClipboardList,
  Gamepad2,
  LayoutDashboard,
  Sparkles,
} from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import ImageLightbox from "@/components/ui/ImageLightbox.vue";
import {
  renderStyledBody,
  githubEmbedDisplayTitle,
  githubEmbedKindLabel,
  githubEmbedNumber,
  extensionSurfaceLabel,
  isAudioFile,
  isImageUrl,
  isImageFile,
  isPdfFile,
  isVideoFile,
  type TimelineMessage,
  type MarkupSpan,
  type MessageReference,
  type TimelineSharedFile,
} from "@/lib/chat-ui";
import { mentionMatchesUsername } from "@/lib/mentions";
import { richMessageToTiptap, tiptapToRichMessage } from "@/lib/rich-message";
import { applyShikiToCodeBlocks } from "@/lib/shiki";
import { decryptEncryptedAttachment, encryptedAttachmentKey, hasEncryptedAttachmentMetadata } from "@/lib/xmpp/encrypted-attachments";
import type { OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatTimeOfDay } from "@/composables/useMessaging";
import { useLongPress } from "@/composables/useLongPress";
import { $desktopToolbarOwnerId } from "@/stores/message-toolbar";

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
  hats: OccupantHat[];
  avatarUrl?: string | null;
  presence?: OccupantPresence;
  lastSeen?: number;
  authorJid?: string;
  threadReplyCount?: number;
  hideThreadChip?: boolean;
  grouped?: boolean;
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]];
  retract: [messageId: string];
  react: [messageId: string, emoji: string];
  reply: [message: TimelineMessage];
  scrollToMessage: [messageId: string];
  avatarClick: [author: string];
  openThread: [threadId: string];
}>();

const quickEmojis = ["👍", "❤️", "😂", "🎉", "👀"];

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

const styledHtml = computed(() => renderStyledBody(displayBody.value, props.message.markup, props.message.references));
const styledBodyRef = ref<HTMLDivElement | null>(null);
const setStyledBodyRef = (el: HTMLDivElement | null) => {
  styledBodyRef.value = el;
};
const sharedFiles = computed(() => props.message.sharedFiles ?? []);
const githubEmbeds = computed(() => props.message.githubEmbeds ?? []);
const extensionAnnotations = computed(() => props.message.extensionAnnotations ?? []);
const isGif = computed(() => sharedFiles.value.length === 0 && isImageUrl(props.message.body));

const imageAttachments = computed(() =>
  sharedFiles.value.filter((f) =>
    f.disposition === "inline" && isImageFile(f.mediaType, f.url),
  ),
);
const videoAttachments = computed(() =>
  sharedFiles.value.filter((f) =>
    f.disposition === "inline"
    && !isImageFile(f.mediaType, f.url)
    && isVideoFile(f.mediaType, f.url),
  ),
);
const audioAttachments = computed(() =>
  sharedFiles.value.filter((f) =>
    f.disposition === "inline"
    && !isImageFile(f.mediaType, f.url)
    && !isVideoFile(f.mediaType, f.url)
    && isAudioFile(f.mediaType, f.url),
  ),
);
const pdfAttachments = computed(() =>
  sharedFiles.value.filter((f) =>
    f.disposition === "inline"
    && !isImageFile(f.mediaType, f.url)
    && !isVideoFile(f.mediaType, f.url)
    && !isAudioFile(f.mediaType, f.url)
    && isPdfFile(f.mediaType, f.url),
  ),
);
const downloadableAttachments = computed(() =>
  sharedFiles.value.filter((f) =>
    f.disposition !== "inline"
    || (
      !isImageFile(f.mediaType, f.url)
      && !isVideoFile(f.mediaType, f.url)
      && !isAudioFile(f.mediaType, f.url)
      && !isPdfFile(f.mediaType, f.url)
    ),
  ),
);
/** Display the user's text body when present; hide body when it's just the fallback URL for a single image. */
const displayBody = computed(() => {
  const body = props.message.body;
  if (!body) return "";
  if (sharedFiles.value.length === 0) return isGif.value ? "" : body;
  const matchesAttachment = sharedFiles.value.some((f) => f.url === body.trim());
  return matchesAttachment ? "" : body;
});

const lightboxOpen = ref(false);
const lightboxIndex = ref(0);
const decryptedAttachmentUrls = ref<Record<string, string>>({});
const decryptedAttachmentErrors = ref<Record<string, string>>({});
const decryptingAttachmentKeys = ref<Record<string, boolean>>({});

function attachmentKey(file: TimelineSharedFile): string {
  return hasEncryptedAttachmentMetadata(file) ? encryptedAttachmentKey(file) : file.url;
}

function setAttachmentFlag(state: typeof decryptingAttachmentKeys, key: string, value: boolean) {
  const next = { ...state.value };
  if (value) next[key] = true;
  else delete next[key];
  state.value = next;
}

function setAttachmentError(key: string, value?: string) {
  const next = { ...decryptedAttachmentErrors.value };
  if (value) next[key] = value;
  else delete next[key];
  decryptedAttachmentErrors.value = next;
}

function revokeAttachmentUrl(key: string) {
  const current = decryptedAttachmentUrls.value[key];
  if (!current) return;
  URL.revokeObjectURL(current);
  const next = { ...decryptedAttachmentUrls.value };
  delete next[key];
  decryptedAttachmentUrls.value = next;
}

function resolvedAttachmentUrl(file: TimelineSharedFile): string | null {
  if (!hasEncryptedAttachmentMetadata(file)) return file.url;
  return decryptedAttachmentUrls.value[attachmentKey(file)] ?? null;
}

function attachmentError(file: TimelineSharedFile): string | null {
  return decryptedAttachmentErrors.value[attachmentKey(file)] ?? null;
}

function isDecryptingAttachment(file: TimelineSharedFile): boolean {
  return !!decryptingAttachmentKeys.value[attachmentKey(file)];
}

async function ensureAttachmentReady(file: TimelineSharedFile, persist = false): Promise<string | null> {
  if (!hasEncryptedAttachmentMetadata(file)) return file.url;
  if (typeof window === "undefined" || typeof URL === "undefined") return null;
  const key = attachmentKey(file);
  const existing = decryptedAttachmentUrls.value[key];
  if (existing) return existing;
  if (decryptingAttachmentKeys.value[key]) return null;

  setAttachmentFlag(decryptingAttachmentKeys, key, true);
  setAttachmentError(key);
  try {
    const blob = await decryptEncryptedAttachment(file);
    const objectUrl = URL.createObjectURL(blob);
    if (!persist) return objectUrl;
    const stillVisible = imageAttachments.value.some((attachment) => attachmentKey(attachment) === key);
    if (!stillVisible) {
      URL.revokeObjectURL(objectUrl);
      return null;
    }
    decryptedAttachmentUrls.value = { ...decryptedAttachmentUrls.value, [key]: objectUrl };
    return objectUrl;
  } catch (error) {
    setAttachmentError(key, error instanceof Error ? error.message : "Couldn't decrypt attachment.");
    return null;
  } finally {
    setAttachmentFlag(decryptingAttachmentKeys, key, false);
  }
}

const previewableAttachments = computed(() => [
  ...imageAttachments.value,
  ...videoAttachments.value,
  ...audioAttachments.value,
  ...pdfAttachments.value,
]);

watch(
  previewableAttachments,
  (attachments) => {
    if (typeof window === "undefined") return;
    const activeKeys = new Set(attachments.map((attachment) => attachmentKey(attachment)));
    for (const key of Object.keys(decryptedAttachmentUrls.value)) {
      if (!activeKeys.has(key)) revokeAttachmentUrl(key);
    }
    for (const attachment of attachments) {
      if (hasEncryptedAttachmentMetadata(attachment)) {
        void ensureAttachmentReady(attachment, true);
      }
    }
  },
  { immediate: true },
);

const lightboxImages = computed(() =>
  imageAttachments.value.flatMap((f) => {
    const resolvedUrl = resolvedAttachmentUrl(f);
    if (!resolvedUrl) return [];
    const img: { url: string; name?: string; width?: number; height?: number } = { url: resolvedUrl };
    if (f.name) img.name = f.name;
    if (f.width) img.width = f.width;
    if (f.height) img.height = f.height;
    return [img];
  }),
);

function openLightbox(file: TimelineSharedFile) {
  const resolvedUrl = resolvedAttachmentUrl(file);
  if (!resolvedUrl) return;
  const index = lightboxImages.value.findIndex((image) => image.url === resolvedUrl);
  if (index < 0) return;
  lightboxIndex.value = index;
  lightboxOpen.value = true;
}

async function downloadAttachment(file: TimelineSharedFile) {
  const downloadUrl = await ensureAttachmentReady(file);
  if (!downloadUrl || typeof document === "undefined") return;
  const link = document.createElement("a");
  link.href = downloadUrl;
  link.download = file.name ?? "attachment";
  link.rel = "noopener noreferrer";
  document.body.appendChild(link);
  link.click();
  link.remove();
  if (hasEncryptedAttachmentMetadata(file) && !decryptedAttachmentUrls.value[attachmentKey(file)]) {
    setTimeout(() => URL.revokeObjectURL(downloadUrl), 60_000);
  }
}

async function highlightMessageCodeBlocks() {
  const el = styledBodyRef.value;
  if (!el) return;
  await applyShikiToCodeBlocks(el);
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

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

function startReplyInThreadFromMenu() {
  // Open the thread first so the follow-up reply lands in the panel's
  // composer. Panel ownership of the reply target means we just need to be
  // sure the panel is in focus; the user can then tap "Reply" in-thread.
  const threadId = props.message.threadId ?? props.message.id;
  emit("openThread", threadId);
  closeSheet();
}

const isMentioned = computed(() => {
  if (props.message.broadcastMention) return true;
  if (!props.currentUser || !props.message.mentions) return false;
  return props.message.mentions.some((mention) => mentionMatchesUsername(mention, props.currentUser));
});
const isForumTopic = computed(() => props.message.forumPostKind === "topic" && !!props.message.forumTitle);
const isForumReply = computed(() => props.message.forumPostKind === "reply");
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
  ownsDesktopToolbarLock.value
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

function onWindowPointerDown(event: PointerEvent) {
  if (sheetOpen.value) return;
  if (!bubbleEl.value) return;
  const target = event.target as Node | null;
  if (target && bubbleEl.value.contains(target)) return;
  closePicker(true);
}

function onWindowKeydown(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  if (sheetOpen.value) closeSheet();
  else closePicker(true);
}

// Only listen globally while an overlay is actually open. Otherwise every
// MessageCard in a long timeline would attach its own capture-phase handler
// and run on every pointerdown anywhere on the page.
watch(
  anyOverlayOpen,
  (open) => {
    if (typeof window === "undefined") return;
    if (open) {
      window.addEventListener("pointerdown", onWindowPointerDown, true);
      window.addEventListener("keydown", onWindowKeydown);
    } else {
      window.removeEventListener("pointerdown", onWindowPointerDown, true);
      window.removeEventListener("keydown", onWindowKeydown);
    }
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
  if (typeof URL !== "undefined") {
    for (const key of Object.keys(decryptedAttachmentUrls.value)) {
      revokeAttachmentUrl(key);
    }
  }
  if (typeof window === "undefined") return;
  window.removeEventListener("pointerdown", onWindowPointerDown, true);
  window.removeEventListener("keydown", onWindowKeydown);
});

watch(
  styledHtml,
  () => {
    void nextTick().then(highlightMessageCodeBlocks);
  },
  { immediate: true },
);
</script>

<template>
  <!-- Retracted tombstone -->
  <div
    v-if="message.isRetracted"
    :data-message-id="message.id"
    class="chat-message-grid opacity-35 animate-message-in"
    :class="grouped ? 'chat-message-grouped' : ''"
  >
    <div v-if="grouped" class="chat-message-avatar-cell chat-message-time-gutter">
      <span class="type-meta type-numeric text-muted-foreground/60">{{ formatTimeOfDay(message.createdAt) }}</span>
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
          {{ formatTimeOfDay(message.createdAt) }}
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
    ]"
    @pointerdown="longPress.handlers.onPointerdown"
    @pointermove="longPress.handlers.onPointermove"
    @pointerup="longPress.handlers.onPointerup"
    @pointercancel="longPress.handlers.onPointercancel"
    @pointerleave="longPress.handlers.onPointerleave"
    @contextmenu="onBubbleContextMenu"
  >
    <div v-if="grouped" class="chat-message-avatar-cell chat-message-time-gutter" aria-hidden="true">
      <span class="type-meta type-numeric text-muted-foreground/60">{{ formatTimeOfDay(message.createdAt) }}</span>
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
          {{ formatTimeOfDay(message.createdAt) }}
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

    <!-- Sticker -->
    <div v-else-if="message.isSticker && imageAttachments.length > 0">
      <img
        :src="imageAttachments[0].url"
        :alt="imageAttachments[0].desc ?? message.body ?? 'Sticker'"
        class="max-w-28 max-h-28 rounded-lg object-contain"
        loading="lazy"
      />
    </div>

    <div v-else class="chat-message-media-stack">
      <!-- User text body (shown alongside attachments) -->
      <div
        v-if="displayBody"
        :ref="setStyledBodyRef"
        class="type-message-body break-words styled-body"
        v-html="styledHtml"
      />

      <!-- Inline GIF -->
      <div v-else-if="isGif">
        <img
          :src="message.body.trim()"
          alt="GIF"
          class="chat-attachment-image rounded-lg border border-border object-contain"
          loading="lazy"
        />
      </div>

      <!-- GitHub enrichment cards -->
      <div v-if="githubEmbeds.length > 0" class="flex flex-col gap-2">
        <a
          v-for="(embed, index) in githubEmbeds"
          :key="`${embed.kind}:${embed.url}:${index}`"
          :href="embed.url"
          target="_blank"
          rel="noopener noreferrer"
          class="chat-github-card group/github flex min-w-0 items-center gap-3 rounded-lg border border-border bg-muted/30 p-3 text-left transition-colors hover:bg-muted/55 focus-visible:outline-2 focus-visible:outline-primary"
          :title="embed.url"
        >
          <span class="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-md bg-background text-foreground ring-1 ring-border">
            <Github v-if="embed.kind === 'repo'" class="h-4 w-4" aria-hidden="true" />
            <CircleDot v-else-if="embed.kind === 'issue'" class="h-4 w-4 text-primary/80" aria-hidden="true" />
            <GitPullRequest v-else class="h-4 w-4 text-success" aria-hidden="true" />
          </span>
          <span class="min-w-0 flex-1">
            <span class="type-section-label block text-muted-foreground">
              GitHub {{ githubEmbedKindLabel(embed.kind) }}<template v-if="githubEmbedNumber(embed)"> #{{ githubEmbedNumber(embed) }}</template>
            </span>
            <span class="type-control block truncate text-foreground">
              {{ githubEmbedDisplayTitle(embed) }}
            </span>
          </span>
        </a>
      </div>

      <!-- Waddle extension annotations -->
      <div v-if="extensionAnnotations.length > 0" class="flex flex-col gap-2">
        <div
          v-for="annotation in extensionAnnotations"
          :key="`${annotation.extensionId}:${annotation.annotationId}`"
          class="chat-extension-card flex min-w-0 items-start gap-3 rounded-lg border border-border bg-muted/25 p-3 text-left"
        >
          <span class="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-md bg-background text-foreground ring-1 ring-border">
            <ClipboardList v-if="annotation.surfaceKind === 'board'" class="h-4 w-4 text-primary/80" aria-hidden="true" />
            <Gamepad2 v-else-if="annotation.surfaceKind === 'game'" class="h-4 w-4 text-success" aria-hidden="true" />
            <Bot v-else-if="annotation.surfaceKind === 'chat-bot'" class="h-4 w-4 text-primary/80" aria-hidden="true" />
            <Sparkles v-else-if="annotation.surfaceKind === 'dynamic-canvas'" class="h-4 w-4 text-warning" aria-hidden="true" />
            <LayoutDashboard v-else class="h-4 w-4" aria-hidden="true" />
          </span>
          <span class="min-w-0 flex-1">
            <span class="type-section-label block text-muted-foreground">
              {{ extensionSurfaceLabel(annotation.surfaceKind) }}
            </span>
            <span class="type-control block truncate text-foreground">
              {{ annotation.title }}
            </span>
            <span v-if="annotation.summary" class="type-caption mt-1 block text-muted-foreground">
              {{ annotation.summary }}
            </span>
            <span v-if="annotation.actions.length > 0" class="mt-2 flex flex-wrap gap-2">
              <span
                v-for="action in annotation.actions"
                :key="`${annotation.annotationId}:${action.route}:${action.label}`"
                class="type-caption rounded-md border border-border bg-background px-2 py-1 text-foreground"
              >
                {{ action.label }}
              </span>
            </span>
          </span>
        </div>
      </div>

      <!-- Image attachments gallery -->
      <div v-if="imageAttachments.length > 0" class="chat-attachment-strip">
        <div
          v-for="img in imageAttachments"
          :key="attachmentKey(img)"
          class="rounded-lg border border-border overflow-hidden bg-muted/40"
        >
          <button
            v-if="resolvedAttachmentUrl(img)"
            type="button"
            class="block hover:opacity-90 transition-opacity focus-visible:outline-2 focus-visible:outline-primary"
            :title="img.name ?? 'Image'"
            @click="openLightbox(img)"
          >
            <img
              :src="resolvedAttachmentUrl(img) || ''"
              :alt="img.name ?? 'Shared image'"
              class="chat-attachment-image object-cover"
              loading="lazy"
            />
          </button>
          <div
            v-else
            class="type-caption flex h-36 w-48 flex-col items-center justify-center gap-2 px-4 text-center text-muted-foreground"
          >
            <Lock class="h-4 w-4 text-primary/70" />
            <span>{{ attachmentError(img) ?? (isDecryptingAttachment(img) ? "Decrypting image…" : "Preparing image…") }}</span>
            <button
              v-if="attachmentError(img)"
              type="button"
              class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
              @click="downloadAttachment(img)"
            >
              Download
            </button>
          </div>
          <div
            v-if="img.encrypted"
            class="type-meta type-emphasis flex items-center gap-1 border-t border-border/70 px-2 py-1 text-muted-foreground"
          >
            <Lock class="h-3 w-3 text-primary/70" />
            <span>Encrypted</span>
          </div>
        </div>
      </div>

      <!-- Inline video attachments -->
      <div v-if="videoAttachments.length > 0" class="flex flex-col gap-2">
        <div
          v-for="file in videoAttachments"
          :key="attachmentKey(file)"
          class="chat-attachment-card flex flex-col gap-2 rounded-lg border border-border bg-muted/30 p-2"
        >
          <video
            v-if="resolvedAttachmentUrl(file)"
            :src="resolvedAttachmentUrl(file) || ''"
            class="max-h-72 w-full rounded-lg border border-border bg-black"
            controls
            playsinline
            preload="metadata"
          />
          <div
            v-else
            class="type-caption flex h-40 w-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 text-center text-muted-foreground"
          >
            <Lock class="h-4 w-4 text-primary/70" />
            <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting video…" : "Preparing video…") }}</span>
            <button
              v-if="attachmentError(file)"
              type="button"
              class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
              @click="downloadAttachment(file)"
            >
              Download
            </button>
          </div>
          <div class="type-caption text-muted-foreground">
            {{ file.name ?? "Video" }} · {{ file.mediaType ?? "video" }}
            <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
            <span v-if="file.encrypted"> · Encrypted</span>
          </div>
        </div>
      </div>

      <!-- Inline audio attachments -->
      <div v-if="audioAttachments.length > 0" class="flex flex-col gap-2">
        <div
          v-for="file in audioAttachments"
          :key="attachmentKey(file)"
          class="chat-attachment-card flex flex-col gap-2 rounded-lg border border-border bg-muted/30 p-3"
        >
          <audio
            v-if="resolvedAttachmentUrl(file)"
            :src="resolvedAttachmentUrl(file) || ''"
            class="w-full"
            controls
            preload="metadata"
          />
          <div
            v-else
            class="type-caption flex min-h-20 w-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 text-center text-muted-foreground"
          >
            <Lock class="h-4 w-4 text-primary/70" />
            <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting audio…" : "Preparing audio…") }}</span>
            <button
              v-if="attachmentError(file)"
              type="button"
              class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
              @click="downloadAttachment(file)"
            >
              Download
            </button>
          </div>
          <div class="type-caption text-muted-foreground">
            {{ file.name ?? "Audio" }} · {{ file.mediaType ?? "audio" }}
            <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
            <span v-if="file.encrypted"> · Encrypted</span>
          </div>
        </div>
      </div>

      <!-- Inline PDF attachments -->
      <div v-if="pdfAttachments.length > 0" class="flex flex-col gap-2">
        <div
          v-for="file in pdfAttachments"
          :key="attachmentKey(file)"
          class="chat-attachment-card flex flex-col gap-2 rounded-lg border border-border bg-muted/30 p-2"
        >
          <iframe
            v-if="resolvedAttachmentUrl(file)"
            :src="resolvedAttachmentUrl(file) || ''"
            :title="file.name ?? 'PDF document'"
            class="h-72 w-full rounded-lg border border-border bg-background"
          />
          <div
            v-else
            class="type-caption flex h-40 w-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 text-center text-muted-foreground"
          >
            <Lock class="h-4 w-4 text-primary/70" />
            <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting PDF…" : "Preparing PDF…") }}</span>
            <button
              v-if="attachmentError(file)"
              type="button"
              class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
              @click="downloadAttachment(file)"
            >
              Download
            </button>
          </div>
          <div class="type-caption text-muted-foreground">
            {{ file.name ?? "PDF" }} · {{ file.mediaType ?? "application/pdf" }}
            <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
            <span v-if="file.encrypted"> · Encrypted</span>
          </div>
        </div>
      </div>

      <!-- Downloadable attachments -->
      <div v-if="downloadableAttachments.length > 0" class="flex flex-col gap-1.5">
        <template v-for="file in downloadableAttachments" :key="attachmentKey(file)">
          <button
            v-if="file.encrypted"
            type="button"
            class="chat-file-card inline-flex items-center gap-3 bg-muted rounded-lg p-3 hover:bg-muted/80 transition-all duration-200 text-left"
            @click="downloadAttachment(file)"
          >
            <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
            <div class="flex-1 min-w-0">
              <div class="type-control truncate">{{ file.name ?? "File" }}</div>
              <div class="type-caption flex flex-wrap items-center gap-1.5 text-muted-foreground">
                <span>{{ file.mediaType ?? "file" }}</span>
                <span v-if="file.size">· {{ formatFileSize(file.size) }}</span>
                <span class="inline-flex items-center gap-1 rounded-full bg-primary/8 px-1.5 py-0.5 text-primary/80">
                  <Lock class="h-3 w-3" />
                  Encrypted
                </span>
              </div>
              <div v-if="attachmentError(file)" class="type-caption text-destructive">
                {{ attachmentError(file) }}
              </div>
            </div>
          </button>
          <a
            v-else
            :href="file.url"
            target="_blank"
            rel="noopener noreferrer"
            class="chat-file-card inline-flex items-center gap-3 bg-muted rounded-lg p-3 hover:bg-muted/80 transition-all duration-200"
          >
            <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
            <div class="flex-1 min-w-0">
              <div class="type-control truncate">{{ file.name ?? "File" }}</div>
              <div class="type-caption text-muted-foreground">
                {{ file.mediaType ?? "file" }}
                <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
              </div>
            </div>
          </a>
        </template>
      </div>
    </div>

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
        class="type-caption inline-flex h-7 items-center gap-1 px-2 rounded-lg bg-muted/60 hover:bg-muted transition-all duration-200"
        :title="nicks.join(', ')"
        @click="emit('react', message.id, emoji)"
      >
        <span>{{ emoji }}</span>
        <span class="type-meta type-numeric text-muted-foreground">{{ nicks.length }}</span>
      </button>
    </div>

    <!-- Floating action toolbar — desktop-only hover/focus affordance. On
         touch devices (where hover never fires) long-press opens the action
         sheet instead, so this toolbar stays hidden and we never show two
         emoji rails at once. -->
    <div
      v-if="!isEditing && !desktopToolbarLockedByAnother"
      :class="[
        'absolute -top-4 right-3 flex items-center gap-1 transition-opacity duration-150 bg-card/95 backdrop-blur border border-border rounded-lg shadow-lg p-1 [@media(pointer:coarse)]:hidden',
        desktopToolbarVisibilityClass,
      ]"
    >
      <button
        v-for="e in quickEmojis"
        :key="e"
        type="button"
        class="type-emoji-button h-8 w-8 flex items-center justify-center rounded-md hover:bg-muted hover:scale-105 transition-all duration-150"
        :title="`React with ${e}`"
        :aria-label="`React to message with ${e}`"
        @click="react(e)"
      >{{ e }}</button>
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
