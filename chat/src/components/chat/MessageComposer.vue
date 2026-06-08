<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount } from "vue";
import { Send, Paperclip, FileText, Music4, Puzzle, X, Loader2, Megaphone, Radio, CornerDownLeft } from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import GifPicker from "@/components/chat/GifPicker.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import SlashCommandPopover from "@/components/chat/SlashCommandPopover.vue";
import { searchEmoji } from "@/lib/emoji";
import { getComposerAutocompleteAction, getComposerEscapeAction } from "@/lib/reply-ux";
import { tiptapToRichMessage } from "@/lib/rich-message";
import { extractImagesFromClipboardEvent } from "@/lib/xmpp/file-upload";
import type { MentionCandidate } from "@/lib/mentions";
import { parseSlashTrigger } from "@/lib/slash-trigger";
import { filterSlashCandidates, resolveSlashCommand } from "@/lib/slash-match";
import { buildSlashInvocation, type SlashInvocation } from "@/lib/slash-dispatch";
import type { DiscoveredExtensionCommand } from "@/lib/xmpp/extension-commands";
import { useComposerLinkPreview } from "@/lib/use-composer-link-preview";
import { prepareComposerSendEvent } from "@/lib/composer-send-preparation";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
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
  giphyApiKey: string;
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
  return author.includes("/") ? author.split("/").pop()! : author.split("@")[0] ?? author;
});

const showGifPicker = ref(false);
const showMentions = ref(false);
const showEmoji = ref(false);
const showSlash = ref(false);
const slashPrefix = ref("");
const slashTrailing = ref("");
const dismissedSlashPrefix = ref<string | null>(null);
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
const extensionButtonRef = ref<HTMLButtonElement | null>(null);
const setExtensionButtonRef = (el: HTMLButtonElement | null) => {
  extensionButtonRef.value = el;
};

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

const mentionResults = computed(() => {
  const q = mentionQuery.value.toLowerCase();
  const qNorm = stripDiacritics(mentionQuery.value);
  if (!q) return props.mentionCandidates.slice(0, 8);
  return props.mentionCandidates.filter((candidate) => {
    const lower = candidate.username.toLowerCase();
    return lower.includes(q) || stripDiacritics(candidate.username).includes(qNorm);
  }).slice(0, 8);
});

const emojiResults = computed(() => searchEmoji(emojiQuery.value));
const showForumTitleInput = computed(() => props.isForumChannel && !props.replyingTo);

const slashContext = computed(() => ({ inMuc: !!props.inMuc }));
const showExtensions = computed(() => props.showExtensions !== false);
const slashCandidates = computed(() =>
  filterSlashCandidates(slashPrefix.value, props.slashCommands ?? [], slashContext.value),
);
const slashResolution = computed(() =>
  resolveSlashCommand(slashPrefix.value, props.slashCommands ?? [], slashContext.value),
);
const slashBlocked = computed(() =>
  showSlash.value && slashPrefix.value.length > 0 && !slashResolution.value && slashCandidates.value.length === 0,
);

const activeResults = computed(() => {
  if (showMentions.value) return mentionResults.value;
  if (showEmoji.value) return emojiResults.value;
  if (showSlash.value) return slashCandidates.value;
  return [];
});

const autocompleteAction = computed(() =>
  getComposerAutocompleteAction({
    showMentions: showMentions.value,
    mentionCount: mentionResults.value.length,
    showEmoji: showEmoji.value,
    emojiCount: emojiResults.value.length,
    showSlash: showSlash.value,
    slashHasPrefix: slashPrefix.value.length > 0,
    slashCandidateCount: slashCandidates.value.length,
    slashHasResolution: !!slashResolution.value,
  }),
);

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

function clearAutocomplete() {
  showMentions.value = false;
  showEmoji.value = false;
  showSlash.value = false;
  triggerRange.value = null;
}

function firstParagraphTextFromDoc(doc: any): string {
  const firstChild = doc?.firstChild;
  if (!firstChild || firstChild.type?.name !== "paragraph") return "";
  return firstChild.textContent ?? "";
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
    showSlash.value = false;
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
    showSlash.value = false;
    triggerRange.value = {
      from: pos - emojiMatch[0].trimStart().length,
      to: pos,
    };
    return;
  }
  showEmoji.value = false;

  // Slash autocomplete is anchored to paragraph 0; if the cursor is in a
  // later paragraph, an earlier `/word` must not arm the slash submit path.
  const firstChild = doc.firstChild;
  const cursorInFirstParagraph =
    !!firstChild && firstChild.type?.name === "paragraph" && pos > 0 && pos <= firstChild.nodeSize - 1;

  const firstParagraph = firstParagraphTextFromDoc(doc);
  const slash = parseSlashTrigger(firstParagraph);
  if (slash) {
    // Hold the popover when the cursor is outside paragraph 0 — but keep the
    // dismissedSlashPrefix intact so the user can navigate away and back
    // without re-arming the same `/word` they already dismissed.
    const suppressedByDismissal =
      dismissedSlashPrefix.value !== null && dismissedSlashPrefix.value === slash.prefix;
    if (!cursorInFirstParagraph || suppressedByDismissal) {
      showSlash.value = false;
    } else {
      slashPrefix.value = slash.prefix;
      slashTrailing.value = slash.trailing;
      selectedIndex.value = 0;
      showSlash.value = true;
      // The slash trigger spans the first paragraph from position 1 (after the
      // leading <p> open tag) through the prefix length.
      triggerRange.value = { from: 1, to: 1 + 1 + slash.prefix.length };
    }
    return;
  }

  // The leading `/word` is gone; safe to reset the dismissal too.
  dismissedSlashPrefix.value = null;
  showSlash.value = false;
  triggerRange.value = null;
}

function insertMention(candidate: MentionCandidate) {
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor || !triggerRange.value) return;

  const replacement = `@${candidate.username} `;
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

function expandSlashCandidate(command: DiscoveredExtensionCommand) {
  const tiptapEditor = getTiptapEditor();
  if (!tiptapEditor || !command.composerPrefix) return;
  // If the user already typed a space after the partial prefix, swallow it
  // so we don't end up with double spaces (e.g. `/p ` + `/poll ` → `/poll  `).
  const firstParagraph = firstParagraphTextFromDoc(tiptapEditor.state.doc);
  const consumedExtra = firstParagraph.charAt(1 + slashPrefix.value.length) === " " ? 1 : 0;
  const replacement = `/${command.composerPrefix} `;
  tiptapEditor.chain()
    .focus()
    .setTextSelection({ from: 1, to: 1 + 1 + slashPrefix.value.length + consumedExtra })
    .insertContent(replacement)
    .run();
  slashPrefix.value = command.composerPrefix;
  slashTrailing.value = "";
  showSlash.value = true;
}

function dispatchSlashResolution(): boolean {
  const command = slashResolution.value;
  if (!command) return false;
  // Forum channels demand a title; don't smuggle a slash dispatch past that gate.
  if (showForumTitleInput.value && !forumTitle.value.trim()) return true;
  const invocation = buildSlashInvocation(command, slashTrailing.value);
  const dispatcher = props.dispatchSlashCommand;
  // Hide the popover for the round-trip; only commit the dismissal once the
  // parent reports success, so a failed dispatch leaves slash mode armed for
  // the user to retry, edit, or Esc.
  showSlash.value = false;
  triggerRange.value = null;
  if (!dispatcher) return true;
  const dispatchedPrefix = slashPrefix.value;
  void (async () => {
    const ok = await dispatcher(invocation);
    if (ok) {
      dismissedSlashPrefix.value = dispatchedPrefix;
    }
  })();
  return true;
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

  if (action === "select-command") {
    const candidate = slashCandidates.value[selectedIndex.value];
    if (candidate) expandSlashCandidate(candidate);
    return true;
  }

  if (action === "submit-slash") {
    return dispatchSlashResolution();
  }

  if (action === "block-slash") {
    // Hold the message; popover already shows an inline "no command" hint.
    return true;
  }

  return false;
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
  }
}

function focus() {
  editorRef.value?.focus();
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
    showSlash.value = false;
    dismissedSlashPrefix.value = slashPrefix.value;
    return;
  }

  if (action === "cancel-reply" && !isPreparingSend.value) {
    emit("cancelReply");
  }
}

function onKeydown(e: KeyboardEvent) {
  if (activeResults.value.length > 0 && (showMentions.value || showEmoji.value || showSlash.value)) {
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
  if (isPreparingSend.value) return;
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

      <!-- Pending attachment previews -->
      <div
        v-if="pendingAttachments.length > 0"
        class="chat-composer-attachment-grid animate-fade-in"
      >
        <div
          v-for="att in pendingAttachments"
          :key="att.id"
          class="relative group/att rounded-lg border border-border bg-muted overflow-hidden"
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
            <div class="type-caption px-2 py-1.5">
              <div class="type-emphasis truncate text-foreground">{{ att.name }}</div>
              <div class="text-muted-foreground">{{ formatFileSize(att.size) }}</div>
            </div>
          </div>
          <div v-else-if="att.previewKind === 'audio'" class="flex w-full max-w-72 flex-col gap-2 p-3">
            <div class="type-caption type-emphasis flex items-center gap-2">
              <Music4 class="h-4 w-4 text-primary" />
              <span class="truncate">{{ att.name }}</span>
            </div>
            <audio :src="att.previewUrl" controls class="h-9 w-full" />
            <div class="type-caption text-muted-foreground">
              {{ att.mediaType }} · {{ formatFileSize(att.size) }}
            </div>
          </div>
          <div v-else-if="att.previewKind === 'pdf'" class="w-44">
            <object
              :data="att.previewUrl"
              type="application/pdf"
              class="h-24 w-full bg-background"
            >
              <div class="type-caption flex h-24 w-full flex-col items-center justify-center gap-2 text-muted-foreground">
                <FileText class="h-5 w-5 text-primary" />
                <span>PDF preview</span>
              </div>
            </object>
            <div class="type-caption px-2 py-1.5">
              <div class="type-emphasis truncate text-foreground">{{ att.name }}</div>
              <div class="text-muted-foreground">{{ formatFileSize(att.size) }}</div>
            </div>
          </div>
          <div v-else class="flex w-full max-w-72 items-center gap-3 p-3">
            <FileText class="h-5 w-5 flex-shrink-0 text-primary" />
            <div class="min-w-0 flex-1">
              <div class="type-control truncate text-foreground">{{ att.name }}</div>
              <div class="type-caption truncate text-muted-foreground">
                {{ att.mediaType }} · {{ formatFileSize(att.size) }}
              </div>
            </div>
          </div>
          <button
            type="button"
            class="absolute top-1 right-1 h-6 w-6 flex items-center justify-center rounded-full bg-background/90 text-muted-foreground hover:text-destructive border border-border shadow-sm opacity-0 group-hover/att:opacity-100 focus:opacity-100 transition-opacity"
            :title="`Remove ${att.name}`"
            :aria-label="`Remove attachment ${att.name}`"
            @click="removeAttachment(att.id)"
          >
            <X class="w-3.5 h-3.5" aria-hidden="true" />
          </button>
        </div>
      </div>

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
      :api-key="giphyApiKey"
      :is-top-pinned="isTopPinned"
      @select="onGifSelected"
      @close="showGifPicker = false"
    />



    <!-- @mention autocomplete -->
    <div
      v-if="showMentions && mentionResults.length > 0"
      class="z-popover chat-composer-popover absolute glass-panel border border-border rounded-lg max-h-56 overflow-auto min-w-0 shadow-xl animate-fade-in p-1"
      :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
    >
      <div class="flex flex-col gap-1">
        <button
          v-for="(candidate, i) in mentionResults"
          :key="candidate.kind === 'broadcast' ? `broadcast:${candidate.username}` : candidate.jid ?? candidate.username"
          type="button"
          class="type-control w-full h-9 px-3 py-0 text-left transition-colors flex items-center gap-2 rounded-lg"
          :class="i === selectedIndex
            ? 'bg-primary/15 hover:bg-primary/20'
            : 'hover:bg-muted'"
          @mousedown.prevent="insertMention(candidate)"
        >
          <!-- Broadcast mentions get distinct glyphs so @everyone vs @here
               reads at a glance without scanning the username text:
               Megaphone = announce to everyone, Radio = ping the people
               who are tuned in (online here). Other broadcast values
               (future-proof) fall back to a generic @ mark. -->
          <span
            v-if="candidate.kind === 'broadcast' && candidate.username === 'everyone'"
            class="flex h-5 w-5 items-center justify-center rounded bg-primary/10 text-primary"
          >
            <Megaphone class="h-3 w-3" aria-hidden="true" />
          </span>
          <span
            v-else-if="candidate.kind === 'broadcast' && candidate.username === 'here'"
            class="flex h-5 w-5 items-center justify-center rounded bg-primary/10 text-primary"
          >
            <Radio class="h-3 w-3" aria-hidden="true" />
          </span>
          <span
            v-else-if="candidate.kind === 'broadcast'"
            class="flex h-5 w-5 items-center justify-center rounded bg-primary/10 text-primary type-caption"
          >@</span>
          <img
            v-else-if="candidate.avatar_url"
            :src="candidate.avatar_url"
            :alt="candidate.username"
            class="h-5 w-5 rounded object-cover bg-muted"
            loading="lazy"
          />
          <span
            v-else
            class="type-caption flex h-5 w-5 items-center justify-center rounded bg-muted text-muted-foreground"
          >@</span>
          <span class="type-emphasis">{{ candidate.username }}</span>
        </button>
      </div>
    </div>

    <!-- :emoji autocomplete -->
    <div
      v-if="showEmoji && emojiResults.length > 0"
      class="z-popover chat-composer-popover absolute glass-panel border border-border rounded-lg max-h-56 overflow-auto min-w-0 shadow-xl animate-fade-in p-1"
      :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
    >
      <div class="flex flex-col gap-1">
        <button
          v-for="(entry, i) in emojiResults"
          :key="entry.name"
          type="button"
          class="type-control w-full h-9 px-3 py-0 text-left transition-colors flex items-center gap-2 rounded-lg"
          :class="i === selectedIndex
            ? 'bg-primary/15 hover:bg-primary/20'
            : 'hover:bg-muted'"
          @mousedown.prevent="insertEmoji(entry.emoji)"
        >
          <span>{{ entry.emoji }}</span>
          <span class="type-caption text-muted-foreground">:{{ entry.name }}:</span>
        </button>
      </div>
    </div>

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
