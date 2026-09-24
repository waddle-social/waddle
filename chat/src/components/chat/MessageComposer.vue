<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount, nextTick } from "vue";
import { AtSign, CaseSensitive, CornerDownLeft, FileText, Loader2, Plus, SendHorizontal, Smile, SquareSlash, X } from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import GifPicker from "@/components/chat/GifPicker.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import ComposerAddMenu from "@/components/chat/ComposerAddMenu.vue";
import ComposerAttachmentGrid from "@/components/chat/ComposerAttachmentGrid.vue";
import ComposerFormattingBar from "@/components/chat/ComposerFormattingBar.vue";
import ComposerEmojiPopover from "@/components/chat/ComposerEmojiPopover.vue";
import ComposerMentionPopover from "@/components/chat/ComposerMentionPopover.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import SlashCommandPopover from "@/components/chat/SlashCommandPopover.vue";
import { getComposerEscapeAction } from "@/lib/reply-ux";
import { tiptapToRichMessage } from "@/lib/rich-message";
import { planComposerPaste } from "@/lib/composer-paste/plan-composer-paste";
import { resolveComposerPaste } from "@/lib/composer-paste/resolve-composer-paste";
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
import { composerPlaceholder } from "./composer-placeholder";
import { useComposerAutocomplete } from "./composables/use-composer-autocomplete";
import type { BuiltinSlashOutcome } from "@/lib/slash-builtins";
import { rewriteBuiltinSendDoc } from "@/lib/slash-builtin-doc";
import { pickPresence } from "@/presence/presence-store";

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
  slashCommands?: DiscoveredExtensionCommand[];
  inMuc?: boolean;
  dispatchSlashCommand?: (invocation: SlashInvocation) => Promise<boolean>;
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
  composerLabel?: string;
  showExtensions?: boolean;
  /** Overrides the default `Message #channel` placeholder (DMs, threads). */
  placeholder?: string;
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
const gifPickerQuery = ref("");
const showAddMenu = ref(false);
const showEmojiPicker = ref(false);
const showFormatting = ref(false);
const editorRef = ref<InstanceType<typeof ChatEditor> | null>(null);
const setEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editorRef.value = instance;
};
const fileInputRef = ref<HTMLInputElement | null>(null);
const setFileInputRef = (el: HTMLInputElement | null) => {
  fileInputRef.value = el;
};
const addButtonRef = ref<HTMLButtonElement | null>(null);
const setAddButtonRef = (el: HTMLButtonElement | null) => {
  addButtonRef.value = el;
};
const emojiButtonRef = ref<HTMLButtonElement | null>(null);
const setEmojiButtonRef = (el: HTMLButtonElement | null) => {
  emojiButtonRef.value = el;
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
  runBuiltinSlash,
});

/** Whether the composer has nothing sendable (no text and no pending attachments). */
const isEmpty = computed(() => !draft.value.trim() && pendingAttachments.value.length === 0);
const isSendBusy = computed(() => props.isSending || isPreparingSend.value);
/** Pastes whose files are still resolving (e.g. fetching an animated GIF). */
const pendingPastes = ref(0);
/** Composer-level holds on any send: busy, disabled, slow mode, or a paste
 * still resolving (sending now would let it land in the next message). */
const isSendHeld = computed(() =>
  isSendBusy.value ||
  props.disabled ||
  props.slowModeCooldown > 0 ||
  pendingPastes.value > 0,
);
const canSend = computed(() =>
  !isSendHeld.value &&
  !isEmpty.value &&
  (!showForumTitleInput.value || !!forumTitle.value.trim()),
);
const editorPlaceholder = computed(() =>
  composerPlaceholder({
    slowModeCooldown: props.slowModeCooldown,
    needsForumTitle: showForumTitleInput.value,
    isForumChannel: props.isForumChannel,
    placeholder: props.placeholder,
    channelName: props.channelName,
  }),
);

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
  await sendDoc(doc);
}

/** Serialize a TipTap doc (plus pending attachments) and emit it as a send. */
async function sendDoc(doc: JSONContent) {
  if (isSendHeld.value) return;
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

/** Empty the editor after a built-in command that does not send a message. */
function clearComposerText() {
  clearAutocomplete();
  editorRef.value?.clear();
  draft.value = "";
}

/**
 * Carry out a client built-in slash command resolved by the autocomplete
 * engine: `/me` and `/shrug` rewrite the draft and send it through the
 * normal path; `/giphy` opens the GIF search; `/away`, `/active` and
 * `/dnd` set the manual presence mode.
 */
function runBuiltinSlash(outcome: BuiltinSlashOutcome) {
  if (outcome.kind === "send") {
    const doc = getTiptapEditor()?.getJSON();
    if (doc) void sendDoc(rewriteBuiltinSendDoc(outcome.rewrite, doc));
    return;
  }
  clearComposerText();
  if (outcome.kind === "open-gif-picker") {
    openGifPicker(outcome.query);
    return;
  }
  pickPresence(outcome.pick);
  refocusAfterSend();
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

/** Return focus to the composer's `+` menu trigger, which is where the
 * extensions launcher lives. */
function focusExtensions() {
  addButtonRef.value?.focus();
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

/** Close every composer popover the toolbar opens, so only one shows. */
function closeToolbarPopovers() {
  showAddMenu.value = false;
  showEmojiPicker.value = false;
  showGifPicker.value = false;
}

function toggleAddMenu() {
  const next = !showAddMenu.value;
  closeToolbarPopovers();
  showAddMenu.value = next;
}

function openGifPicker(query = "") {
  closeToolbarPopovers();
  gifPickerQuery.value = query;
  showGifPicker.value = true;
}

function onAddMenuUpload() {
  closeToolbarPopovers();
  openFilePicker();
}

function onAddMenuExtensions() {
  closeToolbarPopovers();
  emit("openExtensions");
}

function toggleEmojiPicker() {
  const next = !showEmojiPicker.value;
  closeToolbarPopovers();
  showEmojiPicker.value = next;
}

function onEmojiPicked(emoji: string) {
  showEmojiPicker.value = false;
  getTiptapEditor()?.chain().focus().insertContent(emoji).run();
}

function onEmojiPickerClose(reason: "escape" | "outside" | "button") {
  showEmojiPicker.value = false;
  if (reason !== "outside") focus();
}

/** Escape returns focus to the `+` trigger, like a native menu button. */
function onAddMenuClose(reason: "escape" | "tab" | "outside") {
  showAddMenu.value = false;
  if (reason === "escape") addButtonRef.value?.focus();
}

/** Insert `@` at the caret (space-separated from a preceding word) so the
 * mention autocomplete opens, as Slack's `@` button does. */
function startMention() {
  const editor = getTiptapEditor();
  if (!editor) return;
  const { from } = editor.state.selection;
  const before = from > 1 ? editor.state.doc.textBetween(from - 1, from, "\n", "\n") : "";
  const needsSpace = before !== "" && !/\s/.test(before);
  editor.chain().focus().insertContent(needsSpace ? " @" : "@").run();
}

/** Arm slash-command mode: prefix the first paragraph with `/` so the
 * command list opens; any text already typed becomes the arguments. */
function startSlashCommand() {
  const editor = getTiptapEditor();
  if (!editor) return;
  const first = editor.state.doc.firstChild;
  if (!first || first.type.name !== "paragraph") {
    editor.commands.focus();
    return;
  }
  if (first.textContent.startsWith("/")) {
    editor.chain().focus().setTextSelection(2).run();
    return;
  }
  const prefix = first.textContent.length > 0 ? "/ " : "/";
  editor.chain().focus().insertContentAt(1, prefix).setTextSelection(2).run();
}

function toggleFormatting() {
  showFormatting.value = !showFormatting.value;
  focus();
}

function onGifSelected(url: string) {
  if (isPreparingSend.value) return;
  showGifPicker.value = false;
  emit("selectGif", url);
  refocusAfterSend();
}

/** Aborts in-flight pasted-GIF fetches when the composer unmounts or moves
 * to another conversation, so a late result never lands in the wrong draft. */
let pasteAbort = new AbortController();
onBeforeUnmount(() => pasteAbort.abort());
watch(
  () => [props.channelName, props.linkPreviewScope],
  () => {
    pasteAbort.abort();
    pasteAbort = new AbortController();
  },
);

/**
 * Claim pastes that carry files or an animated GIF so ProseMirror does not
 * also insert the clipboard's HTML/text; plain text pastes fall through.
 * Pasted files join the pending attachments and are shared via XEP-0363
 * upload like any picked file.
 */
function onEditorPaste(event: ClipboardEvent): boolean {
  const plan = planComposerPaste(event.clipboardData);
  if (plan.kind === "none") return false;
  if (plan.kind === "files-with-text") {
    if (!isPreparingSend.value) addAttachments(plan.files);
    return false;
  }
  if (isPreparingSend.value) return true;
  pendingPastes.value += 1;
  void resolveComposerPaste(plan, { signal: pasteAbort.signal })
    .then((result) => {
      if (result.kind === "files") {
        if (result.files.length > 0) addAttachments(result.files);
        return;
      }
      getTiptapEditor()?.chain().focus().insertContent({ type: "text", text: result.text }).run();
    })
    .finally(() => {
      pendingPastes.value -= 1;
    });
  return true;
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
  if (preparing) showAddMenu.value = false;
});

</script>

<template>
  <div
    class="chat-composer relative flex-shrink-0 bg-background/75"
    :class="isTopPinned ? 'border-b border-border' : 'border-t border-border'"
    @keydown.capture="onKeydown"
  >
    <div
      v-if="replyingTo || showForumTitleInput || linkPreview.showCard.value || uploadProgress.uploading || pendingPastes > 0"
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

      <output
        v-if="pendingPastes > 0"
        class="type-caption flex items-center gap-2 text-muted-foreground animate-fade-in"
      >
        <Loader2 class="h-3.5 w-3.5 motion-safe:animate-spin" aria-hidden="true" />
        <span>Preparing pasted image…</span>
      </output>

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
      :key="gifPickerQuery"
      :is-top-pinned="isTopPinned"
      :initial-query="gifPickerQuery"
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

    <!-- Slack-style card: optional formatting row, the editor, pending
         attachments, then one action row (add, format, emoji, mention,
         command on the left; send on the right). -->
    <div class="chat-composer-card" :class="{ 'chat-composer-card--disabled': disabled }">
      <input
        :ref="setFileInputRef"
        type="file"
        multiple
        class="hidden"
        :disabled="disabled || isPreparingSend"
        @change="onFileInputChange"
      />
      <ComposerFormattingBar
        v-if="showFormatting && tiptapEditor"
        :editor="tiptapEditor"
        :disabled="disabled || isPreparingSend"
      />
      <div class="chat-composer-editor-slot">
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
          :paste-handler="onEditorPaste"
        />
      </div>
      <div v-if="pendingAttachments.length > 0" class="chat-composer-card-attachments">
        <ComposerAttachmentGrid
          :attachments="pendingAttachments"
          @remove="removeAttachment"
        />
      </div>
      <ComposerAddMenu
        v-if="showAddMenu"
        :anchor-el="addButtonRef"
        :show-extensions="showExtensions"
        :is-top-pinned="isTopPinned"
        @upload="onAddMenuUpload"
        @gif="openGifPicker()"
        @extensions="onAddMenuExtensions"
        @close="onAddMenuClose"
      />
      <div class="chat-composer-toolbar">
        <button
          :ref="setAddButtonRef"
          type="button"
          class="chat-composer-add"
          :class="{ 'chat-composer-add--open': showAddMenu }"
          title="Attach"
          aria-label="Attach"
          aria-haspopup="menu"
          :aria-expanded="showAddMenu"
          :disabled="disabled || isPreparingSend"
          @click="toggleAddMenu"
        >
          <Plus class="h-4 w-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="chat-composer-tool"
          :class="{ 'chat-composer-tool--active': showFormatting }"
          :title="showFormatting ? 'Hide formatting' : 'Show formatting'"
          :aria-label="showFormatting ? 'Hide formatting' : 'Show formatting'"
          :aria-pressed="showFormatting"
          :disabled="disabled || isPreparingSend"
          @click="toggleFormatting"
        >
          <CaseSensitive class="h-5 w-5" aria-hidden="true" />
        </button>
        <button
          :ref="setEmojiButtonRef"
          type="button"
          class="chat-composer-tool"
          :class="{ 'chat-composer-tool--active': showEmojiPicker }"
          title="Emoji"
          aria-label="Emoji"
          aria-haspopup="dialog"
          :aria-expanded="showEmojiPicker"
          :disabled="disabled || isPreparingSend"
          @click="toggleEmojiPicker"
        >
          <Smile class="h-4 w-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="chat-composer-tool"
          title="Mention someone"
          aria-label="Mention someone"
          :disabled="disabled || isPreparingSend"
          @click="startMention"
        >
          <AtSign class="h-4 w-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="chat-composer-tool"
          title="Run a command"
          aria-label="Run a command"
          :disabled="disabled || isPreparingSend"
          @click="startSlashCommand"
        >
          <SquareSlash class="h-4 w-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          class="chat-composer-send"
          :class="{ 'chat-composer-send--armed': canSend }"
          :disabled="!canSend"
          :title="isSendBusy ? 'Sending message' : 'Send now'"
          :aria-label="isSendBusy ? 'Sending message' : 'Send message'"
          :aria-busy="isSendBusy"
          @click="onSend(editorRef?.getJSON?.() ?? { type: 'doc', content: [] })"
        >
          <span v-if="slowModeCooldown > 0" class="type-meta type-numeric type-strong">{{ slowModeCooldown }}</span>
          <Loader2 v-else-if="isSendBusy" class="h-4 w-4 motion-safe:animate-spin" aria-hidden="true" />
          <SendHorizontal v-else class="h-4 w-4" aria-hidden="true" />
        </button>
      </div>
    </div>
    <EmojiPicker
      :open="showEmojiPicker"
      :anchor-el="emojiButtonRef"
      purpose="insert"
      @select="onEmojiPicked"
      @close="onEmojiPickerClose"
    />
    <EditorBubbleToolbar v-if="tiptapEditor" :editor="tiptapEditor" :suppressed="showFormatting" />
  </div>
</template>
