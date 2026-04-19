<script setup lang="ts">
import { ref, computed, nextTick, onBeforeUnmount, watch } from "vue";
import { MoreHorizontal, Pencil, Reply, SmilePlus, Trash2, FileDown, CornerDownRight, MessageSquare, Lock } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import ImageLightbox from "@/components/ui/ImageLightbox.vue";
import { renderStyledBody, isImageUrl, type TimelineMessage, type MarkupSpan, type TimelineSharedFile } from "@/lib/chat-ui";
import { serializeTiptapToXep0393 } from "@/lib/editor/xep0393-serializer";
import { parseXep0393ToTiptap } from "@/lib/editor/xep0393-parser";
import { applyShikiToCodeBlocks } from "@/lib/shiki";
import { decryptEncryptedAttachment, encryptedAttachmentKey, hasEncryptedAttachmentMetadata } from "@/lib/xmpp/encrypted-attachments";
import type { OccupantHat, OccupantPresence } from "@/lib/xmpp-client";
import { formatStamp } from "@/composables/useMessaging";
import { useLongPress } from "@/composables/useLongPress";

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
}>();

const emit = defineEmits<{
  edit: [messageId: string, newBody: string, markup?: MarkupSpan[]];
  retract: [messageId: string];
  react: [messageId: string, emoji: string];
  reply: [message: TimelineMessage];
  scrollToMessage: [messageId: string];
  avatarClick: [author: string];
  openThread: [threadId: string];
}>();

const quickEmojis = ["👍", "❤️", "😂", "🎉", "👀"];

const styledHtml = computed(() => renderStyledBody(displayBody.value, props.message.markup));
const styledBodyRef = ref<HTMLDivElement | null>(null);
const setStyledBodyRef = (el: HTMLDivElement | null) => {
  styledBodyRef.value = el;
};
const sharedFiles = computed(() => props.message.sharedFiles ?? []);
const isGif = computed(() => sharedFiles.value.length === 0 && isImageUrl(props.message.body));

const imageAttachments = computed(() =>
  sharedFiles.value.filter((f) => f.disposition === "inline" && f.mediaType?.startsWith("image/")),
);
const nonImageAttachments = computed(() =>
  sharedFiles.value.filter((f) => !(f.disposition === "inline" && f.mediaType?.startsWith("image/"))),
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

watch(
  imageAttachments,
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
  return props.message.mentions.some(
    (m) => m === props.currentUser || m.split("@")[0] === props.currentUser,
  );
});
const isForumTopic = computed(() => props.message.forumPostKind === "topic" && !!props.message.forumTitle);
const isForumReply = computed(() => props.message.forumPostKind === "reply");
const forumThreadLabel = computed(() =>
  props.message.forumPostKind === "topic"
    ? props.message.forumTitle
    : props.message.forumThreadTitle,
);

const isEditing = ref(false);
const editInitialContent = ref<Record<string, unknown> | undefined>(undefined);
const editEditorRef = ref<InstanceType<typeof ChatEditor> | null>(null);
const setEditEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editEditorRef.value = instance;
};

function startEdit() {
  editInitialContent.value = parseXep0393ToTiptap(props.message.body);
  isEditing.value = true;
}

function cancelEdit() {
  isEditing.value = false;
  editInitialContent.value = undefined;
}

function submitEditFromEditor(doc: Record<string, unknown>) {
  const { body, markup } = serializeTiptapToXep0393(doc);
  const trimmed = body.trim();
  if (trimmed && trimmed !== props.message.body) {
    emit("edit", props.message.id, trimmed, markup);
  }
  isEditing.value = false;
  editInitialContent.value = undefined;
}

function emitAvatarClick() {
  emit("avatarClick", props.message.author);
}

const bubbleEl = ref<HTMLElement | null>(null);
// Inline hover toolbar's SmilePlus popover. Desktop-only; bound to hover.
const pickerOpen = ref(false);
// Unified action sheet: touch long-press and the mobile MoreHorizontal trigger
// open the same surface so there is never more than one emoji rail on screen.
const sheetOpen = ref(false);
type SheetView = "actions" | "emoji";
const sheetView = ref<SheetView>("actions");

const anyOverlayOpen = computed(() => pickerOpen.value || sheetOpen.value);

function closeSheet() {
  sheetOpen.value = false;
  sheetView.value = "actions";
}

function openSheet() {
  pickerOpen.value = false;
  sheetView.value = "actions";
  sheetOpen.value = true;
}

function togglePicker() {
  const next = !pickerOpen.value;
  closeSheet();
  pickerOpen.value = next;
}

function react(emoji: string) {
  emit("react", props.message.id, emoji);
  pickerOpen.value = false;
  closeSheet();
}

function startReplyFromMenu() {
  emit("reply", props.message);
  closeSheet();
}

function startEditFromMenu() {
  startEdit();
  closeSheet();
}

function retractFromMenu() {
  emit("retract", props.message.id);
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
  pickerOpen.value = false;
}

function onWindowKeydown(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  if (sheetOpen.value) closeSheet();
  else pickerOpen.value = false;
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
    class="flow-root px-3 py-2 opacity-30 animate-message-in"
  >
    <AppAvatar
      class="float-left mr-3 mt-0.5"
      :name="message.author"
      :src="avatarUrl"
      :presence="presence"
      :last-seen="lastSeen"
      size="md"
    />
    <div class="flex items-baseline gap-2 mb-0.5">
      <span class="font-medium text-[13px]">{{ message.author }}</span>
      <span class="text-[11px] font-mono text-muted-foreground tabular-nums">
        {{ formatStamp(message.createdAt) }}
      </span>
    </div>
    <p class="text-[13px] italic text-muted-foreground">This message was deleted.</p>
  </div>

  <!-- Normal message -->
  <div
    v-else
    ref="bubbleEl"
    :data-message-id="message.id"
    :data-sheet-open="sheetOpen ? 'true' : 'false'"
    class="group relative flow-root px-3 py-1.5 rounded-xl transition-all duration-200 animate-message-in"
    :class="[
      isMentioned
        ? 'bg-warning/5 border-l-2 border-warning/30'
        : isForumTopic
          ? 'border border-border/70 bg-card/55 shadow-sm hover:bg-card/75'
          : message.threadId
            ? 'border-l-2 border-primary/20 hover:bg-muted/40'
            : 'hover:bg-muted/40',
      message.deliveryStatus === 'sending' || message.deliveryStatus === 'queued' ? 'opacity-50' : '',
      longPress.isPressing.value ? 'no-callout' : '',
    ]"
    @pointerdown="longPress.handlers.onPointerdown"
    @pointermove="longPress.handlers.onPointermove"
    @pointerup="longPress.handlers.onPointerup"
    @pointercancel="longPress.handlers.onPointercancel"
    @pointerleave="longPress.handlers.onPointerleave"
    @contextmenu="onBubbleContextMenu"
  >
    <button
      class="float-left mr-3 mt-0.5 rounded-lg"
      type="button"
      @click.stop="emitAvatarClick"
    >
      <AppAvatar :name="message.author" :src="avatarUrl" :presence="presence" :last-seen="lastSeen" size="md" />
    </button>
    <div class="flex items-baseline gap-2 mb-0.5 flex-wrap">
      <span class="font-semibold text-[13px]">{{ message.author }}</span>
      <span
        v-for="hat in hats"
        :key="hat.uri"
        class="inline-block px-1.5 py-px text-[9px] font-bold uppercase tracking-wider rounded-md leading-none"
        :class="HAT_COLORS[hat.uri] ?? 'bg-muted text-muted-foreground'"
        :title="hat.title"
      >{{ HAT_LABELS[hat.uri] ?? hat.title }}</span>
      <span class="text-[11px] font-mono text-muted-foreground/60 tabular-nums">
        {{ formatStamp(message.createdAt) }}
      </span>
      <span v-if="message.isEdited" class="text-[11px] text-muted-foreground/50">(edited)</span>
      <span
        v-if="message.isSelf && deliveryStatusLabel"
        class="text-[11px]"
        :class="deliveryStatusClass"
      >
        {{ deliveryStatusLabel }}
      </span>
      <span
        v-if="message.isSelf && message.readBy && message.readBy.length > 0"
        class="text-[11px] text-muted-foreground/50"
        :title="message.readBy.join(', ')"
      >
        Read by {{ message.readBy.length }}
      </span>
    </div>

    <div
      v-if="isForumTopic && forumThreadLabel"
      class="mb-2 rounded-xl border border-primary/10 bg-primary/6 px-3 py-2"
    >
      <div class="text-[10px] font-semibold uppercase tracking-[0.14em] text-primary/75">
        Topic
      </div>
      <h3 class="mt-1 text-[15px] font-display font-bold tracking-tight text-foreground">
        {{ forumThreadLabel }}
      </h3>
    </div>
    <div
      v-else-if="isForumReply && forumThreadLabel"
      class="mb-1 inline-flex max-w-full items-center gap-1.5 rounded-full border border-border bg-muted/50 px-2.5 py-1 text-[11px] text-muted-foreground"
    >
      <CornerDownRight class="w-3 h-3 flex-shrink-0 text-primary/70" />
      <span class="truncate">In {{ forumThreadLabel }}</span>
    </div>
    <!-- Reply preview chip. Clicking scrolls to the parent message; if the
         preview is available we also expand it inline so users still see the
         full quoted text even when the parent has scrolled off-screen or
         hasn't loaded from history yet. -->
    <div v-if="message.replyTo" class="mb-0.5">
      <button
        type="button"
        class="flex items-start gap-1.5 text-[11px] text-muted-foreground hover:text-foreground transition-colors max-w-full text-left"
        :aria-expanded="replyChipExpanded"
        :title="message.replyTo.preview ? 'Show full quoted message and jump to it' : 'Jump to replied message'"
        @click="onReplyChipClick"
      >
        <CornerDownRight class="w-3 h-3 flex-shrink-0 mt-0.5" />
        <span class="text-primary/80 font-medium">@{{ replyAuthorName }}</span>
        <span
          v-if="message.replyTo.preview"
          :class="['flex-1 min-w-0 opacity-70', replyChipExpanded ? 'whitespace-pre-wrap break-words' : 'truncate']"
        >{{ message.replyTo.preview }}</span>
        <span v-else class="opacity-60 font-mono">{{ message.replyTo.id.slice(0, 8) }}</span>
      </button>
    </div>

    <!-- Edit mode -->
    <div v-if="isEditing" class="flex gap-2 mt-1 items-end">
      <ChatEditor
        :ref="setEditEditorRef"
        compact
        :initial-content="editInitialContent"
        placeholder="Edit message..."
        @send="submitEditFromEditor"
        @cancel="cancelEdit"
        class="flex-1"
      />
      <button
        class="text-[12px] font-medium px-3 h-9 rounded-xl text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-200 shrink-0"
        @click="cancelEdit"
      >Cancel</button>
    </div>

    <!-- Sticker -->
    <div v-else-if="message.isSticker && imageAttachments.length > 0" class="mt-1">
      <img
        :src="imageAttachments[0].url"
        :alt="imageAttachments[0].desc ?? message.body ?? 'Sticker'"
        class="max-w-28 max-h-28 object-contain"
        loading="lazy"
      />
    </div>

    <template v-else>
      <!-- User text body (shown alongside attachments) -->
      <div
        v-if="displayBody"
        :ref="setStyledBodyRef"
        class="text-[13px] leading-relaxed break-words styled-body"
        v-html="styledHtml"
      />

      <!-- Inline GIF -->
      <div v-else-if="isGif" class="mt-2">
        <img
          :src="message.body.trim()"
          alt="GIF"
          class="max-w-xs max-h-56 rounded-xl border border-border object-contain"
          loading="lazy"
        />
      </div>

      <!-- Image attachments gallery -->
      <div v-if="imageAttachments.length > 0" class="mt-2 flex flex-wrap gap-2">
        <div
          v-for="img in imageAttachments"
          :key="attachmentKey(img)"
          class="rounded-xl border border-border overflow-hidden bg-muted/40"
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
              class="max-w-xs max-h-56 object-cover"
              loading="lazy"
            />
          </button>
          <div
            v-else
            class="flex h-36 w-48 flex-col items-center justify-center gap-2 px-4 text-center text-[12px] text-muted-foreground"
          >
            <Lock class="h-4 w-4 text-primary/70" />
            <span>{{ attachmentError(img) ?? (isDecryptingAttachment(img) ? "Decrypting image…" : "Preparing image…") }}</span>
            <button
              v-if="attachmentError(img)"
              type="button"
              class="rounded-lg border border-border bg-background px-2.5 py-1 text-[11px] text-foreground hover:bg-muted transition-colors"
              @click="downloadAttachment(img)"
            >
              Download
            </button>
          </div>
          <div
            v-if="img.encrypted"
            class="flex items-center gap-1 border-t border-border/70 px-2 py-1 text-[10px] font-medium text-muted-foreground"
          >
            <Lock class="h-3 w-3 text-primary/70" />
            <span>Encrypted</span>
          </div>
        </div>
      </div>

      <!-- Non-image attachments -->
      <div v-if="nonImageAttachments.length > 0" class="mt-2 flex flex-col gap-1.5">
        <template v-for="file in nonImageAttachments" :key="attachmentKey(file)">
          <button
            v-if="file.encrypted"
            type="button"
            class="inline-flex items-center gap-3 bg-muted rounded-xl p-3 hover:bg-muted/80 transition-all duration-200 max-w-md text-left"
            @click="downloadAttachment(file)"
          >
            <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
            <div class="flex-1 min-w-0">
              <div class="text-[13px] font-medium truncate">{{ file.name ?? "File" }}</div>
              <div class="flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground">
                <span>{{ file.mediaType ?? "file" }}</span>
                <span v-if="file.size">· {{ formatFileSize(file.size) }}</span>
                <span class="inline-flex items-center gap-1 rounded-full bg-primary/8 px-1.5 py-0.5 text-primary/80">
                  <Lock class="h-3 w-3" />
                  Encrypted
                </span>
              </div>
              <div v-if="attachmentError(file)" class="mt-1 text-[11px] text-destructive">
                {{ attachmentError(file) }}
              </div>
            </div>
          </button>
          <a
            v-else
            :href="file.url"
            target="_blank"
            rel="noopener noreferrer"
            class="inline-flex items-center gap-3 bg-muted rounded-xl p-3 hover:bg-muted/80 transition-all duration-200 max-w-md"
          >
            <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
            <div class="flex-1 min-w-0">
              <div class="text-[13px] font-medium truncate">{{ file.name ?? "File" }}</div>
              <div class="text-[11px] text-muted-foreground">
                {{ file.mediaType ?? "file" }}
                <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
              </div>
            </div>
          </a>
        </template>
      </div>
    </template>

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
      class="inline-flex items-center gap-1.5 mt-1.5 px-2 py-1 text-[11px] font-medium rounded-lg bg-primary/10 text-primary hover:bg-primary/20 transition-colors"
      :title="`Open thread (${threadReplyCount} ${threadReplyCount === 1 ? 'reply' : 'replies'})`"
      @click="openThreadFromChip"
    >
      <MessageSquare class="w-3 h-3" />
      <span>{{ threadReplyCount }} {{ threadReplyCount === 1 ? "reply" : "replies" }}</span>
    </button>

    <!-- Existing reactions (inline, always visible when present) -->
    <div v-if="message.reactions && Object.keys(message.reactions).length > 0" class="flex flex-wrap gap-1 mt-1.5">
      <button
        v-for="(nicks, emoji) in message.reactions"
        :key="emoji"
        class="inline-flex items-center gap-1 px-2 py-0.5 text-[12px] rounded-lg bg-muted/60 hover:bg-muted transition-all duration-200"
        :title="nicks.join(', ')"
        @click="emit('react', message.id, emoji)"
      >
        <span>{{ emoji }}</span>
        <span class="text-muted-foreground font-mono text-[10px] tabular-nums">{{ nicks.length }}</span>
      </button>
    </div>

    <!-- Floating action toolbar — desktop-only hover/focus affordance. On
         touch devices (where hover never fires) long-press opens the action
         sheet instead, so this toolbar stays hidden and we never show two
         emoji rails at once. -->
    <div
      v-if="!isEditing"
      class="absolute -top-3 right-3 z-10 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100 pointer-events-none group-hover:pointer-events-auto focus-within:pointer-events-auto transition-opacity duration-150 bg-card/95 backdrop-blur border border-border rounded-lg shadow-lg p-1 [@media(pointer:coarse)]:hidden"
    >
      <button
        v-for="e in quickEmojis"
        :key="e"
        type="button"
        class="h-6 w-6 flex items-center justify-center text-[14px] leading-none rounded-md hover:bg-muted hover:scale-110 transition-all duration-150"
        :title="`React with ${e}`"
        :aria-label="`React to message with ${e}`"
        @click="react(e)"
      >{{ e }}</button>
      <div class="relative">
        <button
          type="button"
          class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
          :class="pickerOpen ? 'bg-muted text-foreground' : ''"
          title="Add reaction"
          aria-label="Add reaction"
          :aria-expanded="pickerOpen"
          aria-haspopup="dialog"
          @click="togglePicker"
        >
          <SmilePlus class="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <EmojiPicker
          :open="pickerOpen"
          @select="react"
          @close="pickerOpen = false"
        />
      </div>
      <button
        type="button"
        class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
        title="Reply"
        aria-label="Reply to message"
        @click="startReplyFromMenu"
      >
        <Reply class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
      <template v-if="message.isSelf">
        <div class="w-px h-4 bg-border mx-0.5" />
        <button
          type="button"
          class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-all duration-150"
          title="Edit message"
          aria-label="Edit message"
          @click="startEditFromMenu"
        >
          <Pencil class="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="h-6 w-6 flex items-center justify-center rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-all duration-150"
          title="Delete message"
          aria-label="Delete message"
          @click="retractFromMenu"
        >
          <Trash2 class="w-3.5 h-3.5" aria-hidden="true" />
        </button>
      </template>
    </div>

    <!-- Action-sheet trigger. Touch-only; desktop already has the hover toolbar. -->
    <button
      v-if="!isEditing"
      type="button"
      class="absolute top-1 right-1 z-10 hidden h-6 w-6 [@media(pointer:coarse)]:flex [@media(pointer:coarse)]:h-10 [@media(pointer:coarse)]:w-10 items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted opacity-70 transition-all duration-150"
      title="Message actions"
      aria-label="Message actions"
      :aria-expanded="sheetOpen"
      aria-haspopup="dialog"
      @click="openSheet"
    >
      <MoreHorizontal class="w-3.5 h-3.5 [@media(pointer:coarse)]:w-5 [@media(pointer:coarse)]:h-5" aria-hidden="true" />
    </button>
  </div>

  <!-- Unified action sheet: opened by touch long-press or the MoreHorizontal
       trigger. Teleported so it escapes overflow-hidden
       ancestors; anchored at the bottom on mobile for large touch targets
       and centred when opened from a wider touch viewport. -->
  <Teleport to="body">
    <div
      v-if="sheetOpen"
      class="fixed inset-0 z-[55] flex items-end sm:items-center justify-center animate-fade-in"
      role="presentation"
    >
      <div class="absolute inset-0 bg-background/60 backdrop-blur-sm" @click="closeSheet" />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Message actions"
        class="relative w-full sm:max-w-sm glass-panel border border-border rounded-t-2xl sm:rounded-2xl shadow-2xl animate-slide-up p-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]"
        @pointerdown.stop
      >
        <div class="sm:hidden flex justify-center mb-2">
          <div class="h-1 w-10 rounded-full bg-muted-foreground/30" />
        </div>

        <template v-if="sheetView === 'actions'">
          <div class="grid grid-cols-6 gap-1 mb-3">
            <button
              v-for="e in quickEmojis"
              :key="`sheet-${e}`"
              type="button"
              class="h-12 flex items-center justify-center text-[26px] leading-none rounded-xl hover:bg-muted active:bg-muted transition-colors"
              :aria-label="`React with ${e}`"
              @click="react(e)"
            >{{ e }}</button>
            <button
              type="button"
              class="h-12 flex items-center justify-center rounded-xl text-muted-foreground hover:text-foreground hover:bg-muted active:bg-muted transition-colors"
              aria-label="More reactions"
              @click="sheetView = 'emoji'"
            >
              <SmilePlus class="w-5 h-5" aria-hidden="true" />
            </button>
          </div>

          <button
            type="button"
            class="w-full flex items-center gap-3 px-3 h-12 rounded-xl text-[15px] hover:bg-muted active:bg-muted transition-colors text-left"
            @click="startReplyFromMenu"
          >
            <Reply class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>Reply</span>
          </button>
          <button
            type="button"
            class="w-full flex items-center gap-3 px-3 h-12 rounded-xl text-[15px] hover:bg-muted active:bg-muted transition-colors text-left"
            @click="startReplyInThreadFromMenu"
          >
            <MessageSquare class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>{{ (threadReplyCount ?? 0) > 0 ? "Open thread" : "Reply in thread" }}</span>
          </button>
          <template v-if="message.isSelf">
            <button
              type="button"
              class="w-full flex items-center gap-3 px-3 h-12 rounded-xl text-[15px] hover:bg-muted active:bg-muted transition-colors text-left"
              @click="startEditFromMenu"
            >
              <Pencil class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
              <span>Edit</span>
            </button>
            <button
              type="button"
              class="w-full flex items-center gap-3 px-3 h-12 rounded-xl text-[15px] text-destructive hover:bg-destructive/10 active:bg-destructive/10 transition-colors text-left"
              @click="retractFromMenu"
            >
              <Trash2 class="w-5 h-5" aria-hidden="true" />
              <span>Delete</span>
            </button>
          </template>
          <button
            type="button"
            class="sm:hidden mt-2 w-full h-12 rounded-xl text-[14px] text-muted-foreground hover:bg-muted active:bg-muted transition-colors"
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

<style scoped>
.styled-body :deep(pre.message-code-block) {
  margin: 0.5rem 0;
  padding: 0.75rem 0.9rem;
  border-radius: 0.75rem;
  border: 1px solid var(--border);
  overflow-x: auto;
}

</style>
