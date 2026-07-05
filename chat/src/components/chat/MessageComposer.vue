<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount, nextTick } from "vue";
import { Send, Paperclip, FileText, Puzzle, X, Loader2, CornerDownLeft } from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import GifPicker from "@/components/chat/GifPicker.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import ComposerAttachmentGrid from "@/components/chat/ComposerAttachmentGrid.vue";
import ComposerEmojiPopover from "@/components/chat/ComposerEmojiPopover.vue";
import ComposerMentionPopover from "@/components/chat/ComposerMentionPopover.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import SlashCommandPopover from "@/components/chat/SlashCommandPopover.vue";
import { getComposerEscapeAction } from "@/lib/reply-ux";
import { tiptapToRichMessage } from "@/lib/rich-message";
import { extractImagesFromClipboardEvent } from "@/lib/xmpp/file-upload";
import type { MentionCandidate } from "@/lib/mentions";
import { jidLocalpart } from "@/lib/xmpp/jid";
import type { SlashInvocation } from "@/lib/slash-dispatch";
import type { DiscoveredExtensionCommand } from "@/lib/xmpp/extension-commands";
import { useComposerLinkPreview } from "@/lib/use-composer-link-preview";
import { prepareComposerSendEvent } from "@/lib/composer-send-preparation";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";
import {
  attachmentName,
  attachmentPreviewKind,
  type PendingAttachment,
} from "./composer-attachments";
import { useComposerAutocomplete } from "./composables/use-composer-autocomplete";

const draft = defineModel<string>("draft", { required: true });
const forumTitle = defineModel<string>("forumTitle", { default: "" });

const props = defineProps<{
  channelName: string;
  isForumChannel: boolean;
  isSending: boolean;
  disabled: boolean;
  mentionCandidates: MentionCandidate[];
  slowModeCooldown: number;
  uploadProgress: { uploading: boolean; progress: number; filename: string };
  replyingTo?: { id: string; author: string; preview?: string } | null;
  isTopPinned?: boolean;
  extensionsOpen?: boolean;
  slashCommands?: DiscoveredExtensionCommand[];
  inMuc?: boolean;
  dispatchSlashCommand?: (invocation: SlashInvocation) => Promise<boolean>;
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
  composerLabel?: string;
  showExtensions?: boolean;
}>();

const emit = defineEmits<{
  send: [
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files?: Array<File | Blob>,
    linkPreview?: ComposerLinkPreviewSendPayload,
  ];
  typing: [];
  selectGif: [url: string];
  cancelReply: [];
  openExtensions: [];
}>();

const replyAuthorName = computed(() => {
  const author = props.replyingTo?.author;
  if (!author) return "";
  return author.includes("/") ? author.split("/").pop() ?? author : jidLocalpart(author);
});

const showGifPicker = ref(false);
const editorRef = ref<InstanceType<typeof ChatEditor> | null>(null);
const setEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editorRef.value = instance;
};
const fileInputRef = ref<HTMLInputElement | null>(null);
const setFileInputRef = (el: HTMLInputElement | null) => {
  fileInputRef.value = el;
};
const extensionButtonRef = ref<HTMLButtonElement | null>(null);
const setExtensionButtonRef = (el: HTMLButtonElement | null) => {
  extensionButtonRef.value = el;
};

/** Get the underlying TipTap Editor instance from the ChatEditor ref. */
function getTiptapEditor() {
  const e = editorRef.value as any;
  return e?.editor?.value ?? e?.editor ?? null;
}

const tiptapEditor = computed(() => {
  const e = editorRef.value as any;
  return e?.editor?.value ?? e?.editor ?? null;
});

const pendingAttachments = ref<PendingAttachment[]>([]);
const isPreparingSend = ref(false);
const linkPreview = useComposerLinkPreview(
  draft,
  computed(() => props.linkPreviewLookup),
  computed(() => props.linkPreviewScope),
);

function addAttachments(files: Array<File | Blob>) {
  if (isPreparingSend.value) return;
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

const showForumTitleInput = computed(() => props.isForumChannel && !props.replyingTo);
const showExtensions = computed(() => props.showExtensions !== false);

const {
  showMentions,
  showEmoji,
  showSlash,
  slashPrefix,
  slashBlocked,
  selectedIndex,
  mentionResults,
  emojiResults,
  slashCandidates,
  autocompleteAction,
  checkAutocompleteFromEditor,
  clearAutocomplete,
  dismissSlash,
  insertMention,
  insertEmoji,
  expandSlashCandidate,
  selectAutocompleteResult,
  onKeydown,
} = useComposerAutocomplete({
  getTiptapEditor,
  mentionCandidates: () => props.mentionCandidates,
  slashCommands: () => props.slashCommands ?? [],
  inMuc: () => !!props.inMuc,
  slashSubmitBlocked: () => showForumTitleInput.value && !forumTitle.value.trim(),
  dispatchSlashCommand: () => props.dispatchSlashCommand,
});

/** Whether the composer has nothing sendable (no text and no pending attachments). */
const isEmpty = computed(() => !draft.value.trim() && pendingAttachments.value.length === 0);
const isSendBusy = computed(() => props.isSending || isPreparingSend.value);
const canSend = computed(() =>
  !isSendBusy.value &&
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

async function onSend(doc: JSONContent) {
  if (isPreparingSend.value) return;
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

  isPreparingSend.value = true;
  try {
    // Detach attachments before emitting so re-entry (e.g. Enter burst) cannot
    // double-send them. Revoke preview URLs once ownership transfers to the parent.
    const files = attachments.map((a) => a.file);
    if (attachments.length > 0) {
      pendingAttachments.value = [];
      for (const a of attachments) URL.revokeObjectURL(a.previewUrl);
    }
    const prepared = await prepareComposerSendEvent({
      serialized,
      files,
      linkPreviewForBody: linkPreview.sendPayloadFor,
    });

    emit(
      "send",
      prepared.body,
      prepared.markup,
      prepared.references,
      prepared.files,
      prepared.linkPreview,
    );
  } finally {
    isPreparingSend.value = false;
    refocusAfterSend();
  }
}

function focus() {
  editorRef.value?.focus();
}

/**
 * Return the caret to the composer after a send so the user can keep typing
 * without re-clicking the input. Both send paths pull focus out of the
 * editor: clicking the send button moves focus onto the button, and the
 * brief `isPreparingSend` disable toggles the editor's `contenteditable`
 * off, which blurs it. Wait a tick so the editor is editable again before
 * focusing.
 */
function refocusAfterSend() {
  void nextTick(() => editorRef.value?.focus());
}

function focusExtensions() {
  extensionButtonRef.value?.focus();
}

defineExpose({ addAttachments, focus, focusExtensions });

function openFilePicker() {
  fileInputRef.value?.click();
}

function onFileInputChange(e: Event) {
  const input = e.target as HTMLInputElement | null;
  if (isPreparingSend.value) {
    if (input) input.value = "";
    return;
  }
  if (!input?.files?.length) return;
  addAttachments(Array.from(input.files));
  input.value = "";
}

function onEditorCancel() {
  const action = getComposerEscapeAction({
    showMentions: showMentions.value,
    showEmoji: showEmoji.value,
    showSlash: showSlash.value,
    isReplyingTo: !!props.replyingTo,
  });

  if (action === "dismiss-autocomplete") {
    clearAutocomplete();
    return;
  }

  if (action === "dismiss-slash") {
    dismissSlash();
    return;
  }

  if (action === "cancel-reply" && !isPreparingSend.value) {
    emit("cancelReply");
  }
}

function onGifSelected(url: string) {
  if (isPreparingSend.value) return;
  showGifPicker.value = false;
  emit("selectGif", url);
  refocusAfterSend();
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

watch(isPreparingSend, (preparing) => {
  if (preparing) showGifPicker.value = false;
});

</script>

<template>
  <div
    class="chat-composer relative flex-shrink-0 bg-background/75"
    :class="isTopPinned ? 'border-b border-border' : 'border-t border-border'"
    @keydown.capture="onKeydown"
  >
    <div
      v-if="replyingTo || showForumTitleInput || linkPreview.showCard.value || pendingAttachments.length > 0 || uploadProgress.uploading"
      class="chat-composer-aux-stack"
    >
      <!-- Reply context chip — appears above the composer when the
           user has clicked Reply on a message. Adds a CornerDownLeft
           glyph (the universal "reply" affordance) and a 3 px primary-
           tinted left rail matching the sidebar / active-channel / hover
           toolbar rail language. The preview text italicises so the eye
           reads "Replying to @user — <they said this>" as a quoted
           fragment, not just more chrome. -->
      <div
        v-if="replyingTo"
        class="type-caption flex items-center gap-2 rounded-lg border border-border border-l-[3px] border-l-primary/60 bg-muted/70 px-3 py-1.5 animate-fade-in"
      >
        <CornerDownLeft class="w-3.5 h-3.5 flex-shrink-0 text-primary/75" aria-hidden="true" />
        <span class="text-muted-foreground">Replying to</span>
        <span class="type-emphasis text-primary/90">@{{ replyAuthorName }}</span>
        <span
          v-if="replyingTo.preview"
          class="italic truncate flex-1 text-muted-foreground/85"
        >{{ replyingTo.preview }}</span>
        <button
          type="button"
          class="ml-auto h-8 w-8 flex items-center justify-center rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
          title="Cancel reply"
          aria-label="Cancel reply"
          :disabled="isPreparingSend"
          @click="emit('cancelReply')"
        >
          <X class="w-3.5 h-3.5" />
        </button>
      </div>

      <div
        v-if="linkPreview.showCard.value"
        class="type-caption flex min-w-0 items-center gap-3 rounded-lg border border-border bg-card/70 px-3 py-2 animate-fade-in"
        :aria-busy="linkPreview.state.value.kind === 'loading'"
      >
        <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <Loader2
            v-if="linkPreview.state.value.kind === 'loading'"
            class="h-4 w-4 motion-safe:animate-spin"
            aria-hidden="true"
          />
          <FileText v-else class="h-4 w-4" aria-hidden="true" />
        </div>
        <div class="min-w-0 flex-1">
          <div class="type-emphasis truncate text-foreground">{{ linkPreview.title.value }}</div>
          <div class="truncate text-muted-foreground">{{ linkPreview.description.value }}</div>
        </div>
        <button
          v-if="linkPreview.canDismiss.value"
          type="button"
          class="h-8 w-8 flex items-center justify-center rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
          title="Remove preview"
          aria-label="Remove preview"
          :disabled="isPreparingSend"
          @click="linkPreview.dismiss"
        >
          <X class="h-3.5 w-3.5" aria-hidden="true" />
        </button>
      </div>

      <div
        v-if="showForumTitleInput"
        class="chat-field-stack rounded-lg border border-border bg-card/60 px-3 py-2.5 animate-fade-in"
      >
        <div class="type-section-label text-muted-foreground">
          New topic
        </div>
        <input
          v-model="forumTitle"
          type="text"
          class="type-card-title w-full bg-transparent placeholder:text-muted-foreground/45 focus:outline-none"
          :disabled="disabled || isSendBusy"
          placeholder="Add a clear title"
          aria-label="Topic title"
        />
        <p class="type-caption text-muted-foreground">
          Top-level forum posts need a title.
        </p>
      </div>

      <ComposerAttachmentGrid
        v-if="pendingAttachments.length > 0"
        :attachments="pendingAttachments"
        @remove="removeAttachment"
      />

      <!-- Upload progress bar -->
      <div
        v-if="uploadProgress.uploading"
        class="type-caption flex items-center gap-2 text-muted-foreground animate-fade-in"
      >
        <span class="truncate max-w-40">Uploading {{ uploadProgress.filename }}…</span>
        <div class="flex-1 h-1 bg-muted rounded-full overflow-hidden">
          <div
            class="h-full bg-primary rounded-full transition-all duration-300"
            :style="{ width: `${Math.round(uploadProgress.progress * 100)}%` }"
          />
        </div>
        <span class="type-numeric">{{ Math.round(uploadProgress.progress * 100) }}%</span>
      </div>
    </div>

    <GifPicker
      v-if="showGifPicker"
      :is-top-pinned="isTopPinned"
      @select="onGifSelected"
      @close="showGifPicker = false"
    />

    <ComposerMentionPopover
      v-if="showMentions && mentionResults.length > 0"
      :results="mentionResults"
      :selected-index="selectedIndex"
      :is-top-pinned="isTopPinned"
      @pick="insertMention"
    />

    <ComposerEmojiPopover
      v-if="showEmoji && emojiResults.length > 0"
      :results="emojiResults"
      :selected-index="selectedIndex"
      :is-top-pinned="isTopPinned"
      @pick="insertEmoji"
    />

    <!-- /slash command autocomplete -->
    <SlashCommandPopover
      v-if="showSlash && (slashCandidates.length > 0 || slashBlocked)"
      :candidates="slashCandidates"
      :selected-index="selectedIndex"
      :prefix="slashPrefix"
      :blocked="slashBlocked"
      :is-top-pinned="isTopPinned"
      @pick="expandSlashCandidate"
    />

    <div
      class="chat-composer-input-shell flex min-w-0 flex-nowrap items-center gap-2 bg-muted p-1 transition-all duration-300 has-[:focus]:ring-2 has-[:focus]:ring-primary/30 has-[:focus]:shadow-[0_0_22px_var(--glow-strong)]"
    >
      <input
        :ref="setFileInputRef"
        type="file"
        multiple
        class="hidden"
        :disabled="disabled || isPreparingSend"
        @change="onFileInputChange"
      />
      <!-- LEFT cluster: content-add actions (attach, GIF). The picker
           launched by the second button is GIPHY-only — not a generic
           photo browser — so the affordance is a typographic GIF mark
           rather than a photo icon. -->
      <button
        type="button"
        class="chat-composer-input-action h-9 w-9 shrink-0 flex items-center justify-center transition-all duration-200 text-muted-foreground hover:bg-background/70 hover:text-primary active:scale-[0.94] disabled:opacity-40 disabled:active:scale-100 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
        title="Attach files"
        aria-label="Attach files"
        :disabled="disabled || isPreparingSend"
        @click="openFilePicker"
      >
        <Paperclip class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="chat-composer-input-action h-9 w-9 shrink-0 flex items-center justify-center transition-all duration-200 active:scale-[0.94] disabled:active:scale-100 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
        :class="showGifPicker ? 'bg-background/80 text-primary' : 'text-muted-foreground hover:bg-background/70 hover:text-primary'"
        title="Search GIFs"
        aria-label="Search GIFs"
        :aria-expanded="showGifPicker"
        :disabled="disabled || isPreparingSend"
        @click="showGifPicker = !showGifPicker"
      >
        <span class="chat-composer-gif-badge" aria-hidden="true">GIF</span>
      </button>
      <ChatEditor
        :ref="setEditorRef"
        class="min-w-0"
        embedded
        :placeholder="editorPlaceholder"
        :disabled="disabled || slowModeCooldown > 0 || isPreparingSend"
        :editor-label="composerLabel ?? `${channelName} composer`"
        @send="onSend"
        @update="onEditorUpdate"
        @selection-update="checkAutocompleteFromEditor"
        @cancel="onEditorCancel"
        @paste="onEditorPaste"
      />
      <!-- RIGHT cluster: dispatch actions (extensions menu, send).
           Extensions live next to send so the right edge is consistently
           "fire an action"; the left edge is consistently "add to the
           message draft". -->
      <button
        v-if="showExtensions"
        :ref="setExtensionButtonRef"
        type="button"
        class="chat-composer-input-action h-9 w-9 shrink-0 flex items-center justify-center transition-all duration-200 text-muted-foreground hover:bg-background/70 hover:text-primary active:scale-[0.94] disabled:opacity-40 disabled:active:scale-100 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
        title="Extensions"
        aria-label="Extensions"
        :aria-expanded="extensionsOpen"
        :disabled="disabled || isPreparingSend"
        @click="emit('openExtensions')"
      >
        <Puzzle class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="chat-composer-send chat-composer-input-action h-9 w-9 shrink-0 flex items-center justify-center bg-primary text-primary-foreground transition-all duration-200 disabled:opacity-20 active:scale-[0.94] motion-safe:hover:scale-[1.04] [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
        :class="canSend ? 'chat-composer-send--armed shadow-[0_0_18px_var(--glow-strong)]' : ''"
        :disabled="!canSend"
        :aria-label="isSendBusy ? 'Sending message' : 'Send message'"
        :aria-busy="isSendBusy"
        @click="onSend(editorRef?.getJSON?.() ?? { type: 'doc', content: [] })"
      >
        <span v-if="slowModeCooldown > 0" class="type-meta type-numeric type-strong">{{ slowModeCooldown }}</span>
        <Loader2 v-else-if="isSendBusy" class="w-4 h-4 motion-safe:animate-spin" aria-hidden="true" />
        <Send v-else class="w-4 h-4" aria-hidden="true" />
      </button>
    </div>
    <EditorBubbleToolbar v-if="tiptapEditor" :editor="tiptapEditor" />
  </div>
</template>
