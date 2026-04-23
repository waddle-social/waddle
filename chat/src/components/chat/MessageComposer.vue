<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount } from "vue";
import { Send, Image, Paperclip, FileText, Music4, X } from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import GifPicker from "@/components/chat/GifPicker.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import { searchEmoji } from "@/lib/emoji";
import { getComposerAutocompleteAction, getComposerEscapeAction } from "@/lib/reply-ux";
import { tiptapToRichMessage } from "@/lib/rich-message";
import { extractImagesFromClipboardEvent } from "@/lib/xmpp/file-upload";
import {
  isAudioFile,
  isImageFile,
  isPdfFile,
  isVideoFile,
  type MarkupSpan,
  type MessageReference,
} from "@/lib/chat-ui";

type AttachmentPreviewKind = "image" | "video" | "audio" | "pdf" | "file";

interface PendingAttachment {
  id: string;
  file: File | Blob;
  previewUrl: string;
  name: string;
  mediaType: string;
  size: number;
  previewKind: AttachmentPreviewKind;
}

const draft = defineModel<string>("draft", { required: true });
const forumTitle = defineModel<string>("forumTitle", { default: "" });

const props = defineProps<{
  channelName: string;
  isForumChannel: boolean;
  isSending: boolean;
  disabled: boolean;
  tenorApiKey: string;
  memberNames: string[];
  slowModeCooldown: number;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  replyingTo?: { id: string; author: string; preview?: string } | null;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  send: [body: string, markup: MarkupSpan[], references: MessageReference[], files?: Array<File | Blob>];
  typing: [];
  selectGif: [url: string];
  cancelReply: [];
}>();

const replyAuthorName = computed(() => {
  const author = props.replyingTo?.author;
  if (!author) return "";
  return author.includes("/") ? author.split("/").pop()! : author.split("@")[0] ?? author;
});

const showGifPicker = ref(false);
const showMentions = ref(false);
const showEmoji = ref(false);
const mentionQuery = ref("");
const emojiQuery = ref("");
const selectedIndex = ref(0);
const editorRef = ref<InstanceType<typeof ChatEditor> | null>(null);
const setEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editorRef.value = instance;
};
const fileInputRef = ref<HTMLInputElement | null>(null);
const setFileInputRef = (el: HTMLInputElement | null) => {
  fileInputRef.value = el;
};

const tiptapEditor = computed(() => {
  const e = editorRef.value as any;
  return e?.editor?.value ?? e?.editor ?? null;
});

const pendingAttachments = ref<PendingAttachment[]>([]);

function attachmentName(file: File | Blob): string {
  return file instanceof File && file.name
    ? file.name
    : `attachment-${Date.now()}.bin`;
}

function attachmentPreviewKind(mediaType?: string, name?: string): AttachmentPreviewKind {
  const candidate = name ?? "";
  if (isImageFile(mediaType, candidate)) return "image";
  if (isVideoFile(mediaType, candidate)) return "video";
  if (isAudioFile(mediaType, candidate)) return "audio";
  if (isPdfFile(mediaType, candidate)) return "pdf";
  return "file";
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function addAttachments(files: Array<File | Blob>) {
  const next = pendingAttachments.value.slice();
  for (const file of files) {
    const name = attachmentName(file);
    const mediaType = file.type || "application/octet-stream";
    next.push({
      id: crypto.randomUUID(),
      file,
      name,
      mediaType,
      size: file.size,
      previewKind: attachmentPreviewKind(mediaType, name),
      previewUrl: URL.createObjectURL(file),
    });
  }
  pendingAttachments.value = next;
}

function removeAttachment(id: string) {
  const found = pendingAttachments.value.find((a) => a.id === id);
  if (found) URL.revokeObjectURL(found.previewUrl);
  pendingAttachments.value = pendingAttachments.value.filter((a) => a.id !== id);
}

onBeforeUnmount(() => {
  for (const a of pendingAttachments.value) URL.revokeObjectURL(a.previewUrl);
});

/** Get the underlying TipTap Editor instance from the ChatEditor ref. */
function getTiptapEditor() {
  const e = editorRef.value as any;
  return e?.editor?.value ?? e?.editor ?? null;
}

// Track the ProseMirror position range for the active autocomplete trigger
const triggerRange = ref<{ from: number; to: number } | null>(null);

function stripDiacritics(s: string): string {
  return s.normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase();
}

const BROADCAST_MENTIONS = ["everyone", "here"];

const mentionResults = computed(() => {
  const q = mentionQuery.value.toLowerCase();
  const qNorm = stripDiacritics(mentionQuery.value);
  const allNames = [...BROADCAST_MENTIONS, ...props.memberNames];
  if (!q) return allNames.slice(0, 8);
  return allNames.filter((n) => {
    const lower = n.toLowerCase();
    return lower.includes(q) || stripDiacritics(n).includes(qNorm);
  }).slice(0, 8);
});

const emojiResults = computed(() => searchEmoji(emojiQuery.value));
const showForumTitleInput = computed(() => props.isForumChannel && !props.replyingTo);

const activeResults = computed(() => {
  if (showMentions.value) return mentionResults.value;
  if (showEmoji.value) return emojiResults.value;
  return [];
});

const autocompleteAction = computed(() =>
  getComposerAutocompleteAction({
    showMentions: showMentions.value,
    mentionCount: mentionResults.value.length,
    showEmoji: showEmoji.value,
    emojiCount: emojiResults.value.length,
  }),
);

/** Whether the composer has nothing sendable (no text and no pending attachments). */
const isEmpty = computed(() => !draft.value.trim() && pendingAttachments.value.length === 0);
const canSend = computed(() =>
  !props.isSending &&
  !props.disabled &&
  props.slowModeCooldown <= 0 &&
  !isEmpty.value &&
  (!showForumTitleInput.value || !!forumTitle.value.trim()),
);
const editorPlaceholder = computed(() => {
  if (props.slowModeCooldown > 0) {
    return `Slow mode — wait ${props.slowModeCooldown}s`;
  }
  if (showForumTitleInput.value) {
    return "Write the opening post";
  }
  if (props.isForumChannel) {
    return "Reply in this topic";
  }
  return `Message #${props.channelName}`;
});

function onEditorUpdate(doc: JSONContent) {
  draft.value = tiptapToRichMessage(doc).body;
  emit("typing");
  checkAutocompleteFromEditor();
}

function clearAutocomplete() {
  showMentions.value = false;
  showEmoji.value = false;
  triggerRange.value = null;
}

function checkAutocompleteFromEditor() {
  const editor = editorRef.value;
  if (!editor) return;
  const json = editor.getJSON?.();
  if (!json) return;

  // Access the underlying TipTap editor to get ProseMirror state
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor?.state) return;

  const { selection, doc } = tiptapEditor.state;
  if (!selection.empty) {
    clearAutocomplete();
    return;
  }

  const pos = selection.from;
  const textBefore = doc.textBetween(0, pos, "\n", "\uFFFC");

  const mentionMatch = textBefore.match(/(?:^|\s)@(\S*)$/);
  if (mentionMatch) {
    mentionQuery.value = mentionMatch[1];
    selectedIndex.value = 0;
    showMentions.value = true;
    showEmoji.value = false;
    triggerRange.value = {
      from: pos - mentionMatch[0].trimStart().length,
      to: pos,
    };
    return;
  }
  showMentions.value = false;

  const emojiMatch = textBefore.match(/(?:^|\s):([a-z0-9_+-]*)$/i);
  if (emojiMatch && emojiMatch[1].length >= 2) {
    emojiQuery.value = emojiMatch[1];
    selectedIndex.value = 0;
    showEmoji.value = true;
    triggerRange.value = {
      from: pos - emojiMatch[0].trimStart().length,
      to: pos,
    };
    return;
  }
  clearAutocomplete();
}

function insertMention(username: string) {
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor || !triggerRange.value) return;

  const replacement = `@${username} `;
  tiptapEditor.chain()
    .focus()
    .insertContentAt(triggerRange.value, replacement)
    .run();

  showMentions.value = false;
  triggerRange.value = null;
}

function insertEmoji(emoji: string) {
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor || !triggerRange.value) return;

  const replacement = `${emoji} `;
  tiptapEditor.chain()
    .focus()
    .insertContentAt(triggerRange.value, replacement)
    .run();

  showEmoji.value = false;
  triggerRange.value = null;
}

function selectAutocompleteResult(action = autocompleteAction.value): boolean {
  if (action === "select-mention") {
    insertMention(mentionResults.value[selectedIndex.value]);
    return true;
  }

  if (action === "select-emoji") {
    insertEmoji(emojiResults.value[selectedIndex.value].emoji);
    return true;
  }

  return false;
}

function onSend(doc: JSONContent) {
  const action = autocompleteAction.value;
  if (selectAutocompleteResult(action)) {
    return;
  }
  if (action === "dismiss-autocomplete") clearAutocomplete();

  const serialized = tiptapToRichMessage(doc);
  const text = serialized.body.trim();
  const attachments = pendingAttachments.value;

  if (showForumTitleInput.value && !forumTitle.value.trim()) return;
  if (!text && attachments.length === 0) return;

  // Detach attachments before emitting so re-entry (e.g. Enter burst) cannot
  // double-send them. Revoke preview URLs once ownership transfers to the parent.
  const files = attachments.map((a) => a.file);
  if (attachments.length > 0) {
    pendingAttachments.value = [];
    for (const a of attachments) URL.revokeObjectURL(a.previewUrl);
  }

  emit(
    "send",
    serialized.body,
    serialized.markup,
    serialized.references,
    files.length > 0 ? files : undefined,
  );
}

function focus() {
  editorRef.value?.focus();
}

defineExpose({ addAttachments, focus });

function openFilePicker() {
  fileInputRef.value?.click();
}

function onFileInputChange(e: Event) {
  const input = e.target as HTMLInputElement | null;
  if (!input?.files?.length) return;
  addAttachments(Array.from(input.files));
  input.value = "";
}

function onEditorCancel() {
  const action = getComposerEscapeAction({
    showMentions: showMentions.value,
    showEmoji: showEmoji.value,
    isReplyingTo: !!props.replyingTo,
  });

  if (action === "dismiss-autocomplete") {
    clearAutocomplete();
    return;
  }

  if (action === "cancel-reply") {
    emit("cancelReply");
  }
}

function onKeydown(e: KeyboardEvent) {
  if (activeResults.value.length > 0 && (showMentions.value || showEmoji.value)) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      e.stopPropagation();
      selectedIndex.value = (selectedIndex.value + 1) % activeResults.value.length;
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      selectedIndex.value =
        (selectedIndex.value - 1 + activeResults.value.length) % activeResults.value.length;
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      e.stopPropagation();
      selectAutocompleteResult();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      e.stopPropagation();
      selectAutocompleteResult();
      return;
    }
  }
}

function onGifSelected(url: string) {
  showGifPicker.value = false;
  emit("selectGif", url);
}

function onEditorPaste(e: ClipboardEvent) {
  const files = extractImagesFromClipboardEvent(e);
  if (files.length > 0) {
    e.preventDefault();
    addAttachments(files);
  }
}

// Clear editor content when draft is reset externally (e.g. after successful send)
watch(
  () => draft.value,
  (newVal) => {
    if (newVal === "") clearAutocomplete();
    if (newVal === "" && editorRef.value && !editorRef.value.isEmpty()) {
      editorRef.value.clear();
    }
  },
);
</script>

<template>
  <div class="relative px-4 py-3 flex-shrink-0" :class="isTopPinned ? 'border-b border-border' : ''" @keydown.capture="onKeydown">
    <!-- Reply context chip -->
    <div
      v-if="replyingTo"
      class="flex items-center gap-2 px-3 py-1.5 mb-2 rounded-xl bg-muted/70 border border-border text-[12px] animate-fade-in"
    >
      <span class="text-muted-foreground">Replying to</span>
      <span class="font-medium text-primary/90">@{{ replyAuthorName }}</span>
      <span v-if="replyingTo.preview" class="text-muted-foreground truncate flex-1">{{ replyingTo.preview }}</span>
      <button
        type="button"
        class="ml-auto h-5 w-5 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
        title="Cancel reply"
        aria-label="Cancel reply"
        @click="emit('cancelReply')"
      >
        <X class="w-3 h-3" />
      </button>
    </div>

    <div
      v-if="showForumTitleInput"
      class="mb-2 rounded-xl border border-border bg-card/60 px-3 py-2.5 animate-fade-in"
    >
      <div class="mb-1.5 text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground">
        New topic
      </div>
      <input
        v-model="forumTitle"
        type="text"
        class="w-full bg-transparent text-[14px] font-medium placeholder:text-muted-foreground/45 focus:outline-none"
        :disabled="disabled || isSending"
        placeholder="Add a clear title"
      />
      <p class="mt-1 text-[11px] text-muted-foreground">
        Top-level forum posts need a title.
      </p>
    </div>

    <!-- Pending attachment previews -->
    <div
      v-if="pendingAttachments.length > 0"
      class="flex flex-wrap gap-2 mb-2 animate-fade-in"
    >
      <div
        v-for="att in pendingAttachments"
        :key="att.id"
        class="relative group/att rounded-xl border border-border bg-muted overflow-hidden"
      >
        <img
          v-if="att.previewKind === 'image'"
          :src="att.previewUrl"
          :alt="att.name"
          class="h-20 w-20 object-cover"
        />
        <div v-else-if="att.previewKind === 'video'" class="w-44">
          <video
            :src="att.previewUrl"
            class="h-24 w-full bg-black object-cover"
            controls
            muted
            playsinline
            preload="metadata"
          />
          <div class="px-2 py-1.5 text-[11px]">
            <div class="truncate font-medium text-foreground">{{ att.name }}</div>
            <div class="text-muted-foreground">{{ formatFileSize(att.size) }}</div>
          </div>
        </div>
        <div v-else-if="att.previewKind === 'audio'" class="w-72 p-3">
          <div class="mb-2 flex items-center gap-2 text-[12px] font-medium">
            <Music4 class="h-4 w-4 text-primary" />
            <span class="truncate">{{ att.name }}</span>
          </div>
          <audio :src="att.previewUrl" controls class="h-9 w-full" />
          <div class="mt-1 text-[11px] text-muted-foreground">
            {{ att.mediaType }} · {{ formatFileSize(att.size) }}
          </div>
        </div>
        <div v-else-if="att.previewKind === 'pdf'" class="w-44">
          <object
            :data="att.previewUrl"
            type="application/pdf"
            class="h-24 w-full bg-background"
          >
            <div class="flex h-24 w-full flex-col items-center justify-center gap-2 text-[12px] text-muted-foreground">
              <FileText class="h-5 w-5 text-primary" />
              <span>PDF preview</span>
            </div>
          </object>
          <div class="px-2 py-1.5 text-[11px]">
            <div class="truncate font-medium text-foreground">{{ att.name }}</div>
            <div class="text-muted-foreground">{{ formatFileSize(att.size) }}</div>
          </div>
        </div>
        <div v-else class="flex w-72 items-center gap-3 p-3">
          <FileText class="h-5 w-5 flex-shrink-0 text-primary" />
          <div class="min-w-0 flex-1">
            <div class="truncate text-[12px] font-medium text-foreground">{{ att.name }}</div>
            <div class="truncate text-[11px] text-muted-foreground">
              {{ att.mediaType }} · {{ formatFileSize(att.size) }}
            </div>
          </div>
        </div>
        <button
          type="button"
          class="absolute top-1 right-1 h-5 w-5 flex items-center justify-center rounded-full bg-background/90 text-muted-foreground hover:text-destructive border border-border shadow-sm opacity-0 group-hover/att:opacity-100 focus:opacity-100 transition-opacity"
          :title="`Remove ${att.name}`"
          :aria-label="`Remove attachment ${att.name}`"
          @click="removeAttachment(att.id)"
        >
          <X class="w-3 h-3" aria-hidden="true" />
        </button>
      </div>
    </div>

    <!-- Upload progress bar -->
    <div
      v-if="uploadProgress.uploading"
      class="flex items-center gap-2 text-[11px] text-muted-foreground mb-2 animate-fade-in"
    >
      <span class="truncate max-w-40">Uploading {{ uploadProgress.filename }}...</span>
      <div class="flex-1 h-1 bg-muted rounded-full overflow-hidden">
        <div
          class="h-full bg-primary rounded-full transition-all duration-300"
          :style="{ width: `${Math.round(uploadProgress.progress * 100)}%` }"
        />
      </div>
      <span class="tabular-nums font-mono">{{ Math.round(uploadProgress.progress * 100) }}%</span>
    </div>

    <GifPicker
      v-if="showGifPicker"
      :api-key="tenorApiKey"
      :is-top-pinned="isTopPinned"
      @select="onGifSelected"
      @close="showGifPicker = false"
    />

    <!-- @mention autocomplete -->
    <div
      v-if="showMentions && mentionResults.length > 0"
      class="absolute left-[4.125rem] right-[4.125rem] glass-panel border border-border rounded-lg max-h-48 overflow-auto z-50 min-w-0 shadow-xl animate-fade-in"
      :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
    >
      <div class="py-1">
        <button
          v-for="(name, i) in mentionResults"
          :key="name"
          class="w-full px-3 py-2 text-left text-[13px] hover:bg-muted transition-colors flex items-center gap-2 rounded-lg"
          :class="i === selectedIndex ? 'bg-muted' : ''"
          @mousedown.prevent="insertMention(name)"
        >
          <span class="text-primary text-[11px]">@</span>
          <span class="font-medium">{{ name }}</span>
        </button>
      </div>
    </div>

    <!-- :emoji autocomplete -->
    <div
      v-if="showEmoji && emojiResults.length > 0"
      class="absolute left-[4.125rem] right-[4.125rem] glass-panel border border-border rounded-lg max-h-48 overflow-auto z-50 min-w-0 shadow-xl animate-fade-in"
      :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
    >
      <div class="py-1">
        <button
          v-for="(entry, i) in emojiResults"
          :key="entry.name"
          class="w-full px-3 py-2 text-left text-[13px] hover:bg-muted transition-colors flex items-center gap-2 rounded-lg"
          :class="i === selectedIndex ? 'bg-muted' : ''"
          @mousedown.prevent="insertEmoji(entry.emoji)"
        >
          <span>{{ entry.emoji }}</span>
          <span class="text-muted-foreground text-[11px]">:{{ entry.name }}:</span>
        </button>
      </div>
    </div>

    <div class="flex min-w-0 flex-nowrap items-end gap-2.5">
      <input
        :ref="setFileInputRef"
        type="file"
        multiple
        class="hidden"
        @change="onFileInputChange"
      />
      <button
        type="button"
        class="h-10 w-10 shrink-0 flex items-center justify-center rounded-lg transition-all duration-200 text-muted-foreground hover:bg-muted hover:text-primary"
        title="Attach files"
        aria-label="Attach files"
        :disabled="disabled"
        @click="openFilePicker"
      >
        <Paperclip class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="h-10 w-10 shrink-0 flex items-center justify-center rounded-lg transition-all duration-200"
        :class="showGifPicker ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-primary'"
        title="Open GIF picker"
        aria-label="Open GIF picker"
        :disabled="disabled"
        @click="showGifPicker = !showGifPicker"
      >
        <Image class="w-4 h-4" aria-hidden="true" />
      </button>
      <ChatEditor
        :ref="setEditorRef"
        class="min-w-0"
        :placeholder="editorPlaceholder"
        :disabled="disabled || slowModeCooldown > 0"
        @send="onSend"
        @update="onEditorUpdate"
        @selection-update="checkAutocompleteFromEditor"
        @cancel="onEditorCancel"
        @paste="onEditorPaste"
      />
      <button
        type="button"
        class="h-10 w-10 shrink-0 flex items-center justify-center bg-primary text-primary-foreground rounded-lg hover:shadow-[0_0_20px_var(--glow-strong)] transition-all duration-300 disabled:opacity-20"
        :disabled="!canSend"
        aria-label="Send message"
        @click="onSend(editorRef?.getJSON?.() ?? { type: 'doc', content: [] })"
      >
        <span v-if="slowModeCooldown > 0" class="text-[10px] font-bold font-mono tabular-nums">{{ slowModeCooldown }}</span>
        <Send v-else class="w-4 h-4" aria-hidden="true" />
      </button>
    </div>
    <EditorBubbleToolbar v-if="tiptapEditor" :editor="tiptapEditor" />
  </div>
</template>
