<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount, nextTick, inject } from "vue";
import { roomLinksKey } from "@/lib/room-links";
import { mentionDirectoryKey } from "@/shell/controllers/use-mention-directory";
import { AtSign, CaseSensitive, CornerDownLeft, FileText, Hash, Loader2, Plus, SendHorizontal, Smile, SquareSlash, X } from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import { css } from "styled-system/css";
import { composer, iconbtn, kicker } from "styled-system/recipes";
import GifPicker from "@/components/chat/GifPicker.vue";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import ComposerAddMenu from "@/components/chat/ComposerAddMenu.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
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
import { jidLocalpart, resourceOf } from "@/lib/xmpp/jid";
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
import { composerPopover, spin } from "./composer-classes";
import { useComposerAutocomplete } from "./composables/use-composer-autocomplete";
import type { BuiltinSlashOutcome } from "@/lib/slash-builtins";
import { rewriteBuiltinSendDoc } from "@/lib/slash-builtin-doc";
import { pickPresence } from "@/presence/presence-store";

const roomPickerLegend = css({ srOnly: true });

const draft = defineModel<string>("draft", { required: true });
const forumTitle = defineModel<string>("forumTitle", { default: "" });

const props = withDefaults(defineProps<{
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
}>(), {
  // An omitted boolean prop resolves to `false` in Vue, so surfaces opt out
  // of the extensions launcher explicitly instead.
  showExtensions: true,
});

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
  return author.includes("/") ? resourceOf(author) || author : jidLocalpart(author);
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
// Provided by the chat shell; the controller only searches in rooms.
const mentionDirectory = inject(mentionDirectoryKey, null);

const {
  showMentions,
  roomResults,
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
  insertRoomLink,
  insertEmoji,
  expandSlashCandidate,
  selectAutocompleteResult,
  onKeydown,
} = useComposerAutocomplete({
  getTiptapEditor,
  mentionCandidates: () => props.mentionCandidates,
  roomCandidates: inject(roomLinksKey)?.candidates,
  searchMentionDirectory: mentionDirectory ? (query) => mentionDirectory.search(query) : undefined,
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
    showMentions: showMentions.value || roomResults.value.length > 0,
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


const cls = {
  // The composer row under the timeline (Room / DM / Mobile Room boards).
  // A size container so the card fits narrow panels (call dock, thread) on wide screens.
  root: css({ position: "relative", flex: "none", px: "6", containerType: "inline-size", _narrow: { px: "3", bg: "bg.rail" } }),
  rootBottom: css({ pb: "5", _narrow: { pt: "2", pb: "max({spacing.3}, env(safe-area-inset-bottom))", borderTop: "1px solid {colors.border.soft}" } }),
  rootTop: css({ pt: "4", _narrow: { pt: "2", pb: "2", borderBottom: "1px solid {colors.border.soft}" } }),
  aux: css({ display: "flex", flexDirection: "column", gap: "2", pb: "2" }),
  chip: css({
    display: "flex",
    minWidth: 0,
    alignItems: "center",
    gap: "2",
    px: "3",
    py: "1.5",
    borderRadius: "control",
    border: "1px solid {colors.border.soft}",
    bg: "surface",
    textStyle: "caption",
    animation: "wd-fade {durations.base} {easings.out} backwards",
  }),
  reply: css({ boxShadow: "inset 3px 0 0 {colors.action.text}" }),
  replyIcon: css({ width: "14px", height: "14px", flex: "none", color: "action.text" }),
  replyName: css({ fontWeight: 700, color: "action.text" }),
  replyPreview: css({ flex: 1, minWidth: 0, fontStyle: "italic", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  muted: css({ margin: 0, color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  pushEnd: css({ ml: "auto" }),
  previewIcon: css({ display: "flex", alignItems: "center", justifyContent: "center", width: "32px", height: "32px", flex: "none", borderRadius: "sm", bg: "surface.2", color: "fg.muted", "& svg": { width: "16px", height: "16px" } }),
  previewText: css({ minWidth: 0, flex: 1 }),
  previewTitle: css({ fontWeight: 700, color: "fg", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  topic: css({ flexDirection: "column", alignItems: "stretch", gap: "1", py: "2.5" }),
  topicInput: css({ width: "100%", border: 0, bg: "transparent", color: "fg", font: "700 17px/1.3 {fonts.display}", outline: "none", _placeholder: { color: "fg.soft" } }),
  status: css({ display: "flex", alignItems: "center", gap: "2", textStyle: "caption", color: "fg.muted", "& svg": { width: "14px", height: "14px" } }),
  uploadName: css({ maxWidth: "160px", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  track: css({ flex: 1, height: "4px", borderRadius: "pill", bg: "surface.2", overflow: "hidden" }),
  bar: css({ height: "100%", borderRadius: "pill", bg: "action", transition: "width {durations.slow} {easings.out}" }),
  num: css({ fontVariantNumeric: "tabular-nums", fontWeight: 700, fontSize: "xs" }),
  // The field: formatting bar and attachments wrap onto their own rows above the editor.
  card: css({ flexWrap: "wrap", "& button:disabled": { opacity: 0.4, cursor: "default" } }),
  cardDisabled: css({ opacity: 0.7 }),
  // Too narrow for editor and tools on one row: the editor takes its own row above them.
  editor: css({ flex: "1 1 0", minWidth: 0, alignSelf: "center", "@container (max-width: 340px)": { order: -2, flexBasis: "100%" } }),
  add: css({ order: -1, "& svg": { transition: "rotate {durations.base} {easings.spring.bouncy}" }, _expanded: { bg: "state.selected", color: "fg", "& svg": { rotate: "45deg" } } }),
  // Phones and narrow panels keep +, emoji and send (Mobile Room board); @, / and the selection toolbar cover the rest.
  wide: css({ _narrow: { display: "none" }, "@container (max-width: 480px)": { display: "none" } }),
  send: css({ "@container (max-width: 340px)": { ml: "auto" } }),
  tool: css({ _expanded: { bg: "state.selected", color: "fg" } }),
  // Send sleeps until there is something to send.
  sendIdle: css({ bg: "transparent", bgImage: "none", boxShadow: "none", color: "fg.soft", _hover: { bg: "state.hover", bgImage: "none", color: "fg.muted" } }),
};
</script>

<template>
  <div
    :class="['chat-composer', cls.root, isTopPinned ? cls.rootTop : cls.rootBottom]"
    @keydown.capture="onKeydown"
  >
    <div
      v-if="replyingTo || showForumTitleInput || linkPreview.showCard.value || uploadProgress.uploading || pendingPastes > 0"
      :class="cls.aux"
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
        :class="[cls.chip, cls.reply]"
      >
        <CornerDownLeft :class="cls.replyIcon" aria-hidden="true" />
        <span :class="cls.muted">Replying to</span>
        <span :class="cls.replyName">@{{ replyAuthorName }}</span>
        <span
          v-if="replyingTo.preview"
          :class="cls.replyPreview"
        >{{ replyingTo.preview }}</span>
        <AppTooltip label="Cancel reply">
          <button
            type="button"
            :class="[iconbtn({ size: 'sm' }), cls.pushEnd]"
            aria-label="Cancel reply"
            :disabled="isPreparingSend"
            @click="emit('cancelReply')"
          >
            <X aria-hidden="true" />
          </button>
        </AppTooltip>
      </div>

      <div
        v-if="linkPreview.showCard.value"
        :class="cls.chip"
        :aria-busy="linkPreview.state.value.kind === 'loading'"
      >
        <div :class="cls.previewIcon">
          <Loader2
            v-if="linkPreview.state.value.kind === 'loading'"
            :class="spin"
            aria-hidden="true"
          />
          <FileText v-else aria-hidden="true" />
        </div>
        <div :class="cls.previewText">
          <div :class="cls.previewTitle">{{ linkPreview.title.value }}</div>
          <div :class="cls.muted">{{ linkPreview.description.value }}</div>
        </div>
        <AppTooltip v-if="linkPreview.canDismiss.value" label="Remove preview">
          <button
            type="button"
            :class="iconbtn({ size: 'sm' })"
            aria-label="Remove preview"
            :disabled="isPreparingSend"
            @click="linkPreview.dismiss"
          >
            <X aria-hidden="true" />
          </button>
        </AppTooltip>
      </div>

      <div
        v-if="showForumTitleInput"
        :class="[cls.chip, cls.topic]"
      >
        <div :class="kicker()">
          New topic
        </div>
        <input
          v-model="forumTitle"
          type="text"
          :class="cls.topicInput"
          :disabled="disabled || isSendBusy"
          placeholder="Add a clear title"
          aria-label="Topic title"
        />
        <p :class="cls.muted">
          Top-level forum posts need a title.
        </p>
      </div>

      <output
        v-if="pendingPastes > 0"
        :class="cls.status"
      >
        <Loader2 :class="spin" aria-hidden="true" />
        <span>Preparing pasted image…</span>
      </output>

      <!-- Upload progress bar -->
      <div
        v-if="uploadProgress.uploading"
        :class="cls.status"
      >
        <span :class="cls.uploadName">Uploading {{ uploadProgress.filename }}…</span>
        <div :class="cls.track">
          <div
            :class="cls.bar"
            :style="{ width: `${Math.round(uploadProgress.progress * 100)}%` }"
          />
        </div>
        <span :class="cls.num">{{ Math.round(uploadProgress.progress * 100) }}%</span>
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

    <fieldset
      v-if="roomResults.length > 0"
      :class="[composerPopover.root, isTopPinned ? composerPopover.below : composerPopover.above]"
    >
      <legend :class="roomPickerLegend">Link a room</legend>
      <button
        v-for="(room, index) in roomResults"
        :key="room.jid"
        type="button"
        :title="room.jid"
        :class="composerPopover.option"
        :data-selected="index === selectedIndex || undefined"
        @mousedown.prevent
        @click.stop="insertRoomLink(room)"
      >
        <span :class="composerPopover.glyph"><Hash aria-hidden="true" /></span>
        <span>{{ room.label }}</span>
        <span
          v-if="roomResults.some((other) => other !== room && other.label === room.label)"
          :class="composerPopover.hint"
        >{{ room.jid }}</span>
      </button>
    </fieldset>

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
    <div :class="[composer(), cls.card, disabled && cls.cardDisabled]" :data-disabled="disabled || undefined">
      <input
        :ref="setFileInputRef"
        type="file"
        multiple
        hidden
        :disabled="disabled || isPreparingSend"
        @change="onFileInputChange"
      />
      <ComposerFormattingBar
        v-if="showFormatting && tiptapEditor"
        :editor="tiptapEditor"
        :disabled="disabled || isPreparingSend"
      />
        <ChatEditor
          :ref="setEditorRef"
          :class="cls.editor"
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
      <ComposerAttachmentGrid
        v-if="pendingAttachments.length > 0"
        :attachments="pendingAttachments"
        @remove="removeAttachment"
      />
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
        <AppTooltip label="Attach">
          <button
            :ref="setAddButtonRef"
            type="button"
            :class="[iconbtn(), cls.add]"
            aria-label="Attach"
            aria-haspopup="menu"
            :aria-expanded="showAddMenu"
            :disabled="disabled || isPreparingSend"
            @click="toggleAddMenu"
          >
            <Plus aria-hidden="true" />
          </button>
        </AppTooltip>
        <AppTooltip :label="showFormatting ? 'Hide formatting' : 'Show formatting'">
          <button
            type="button"
            :class="[iconbtn(), cls.wide]"
            data-composer-wide
            :aria-label="showFormatting ? 'Hide formatting' : 'Show formatting'"
            :aria-pressed="showFormatting"
            :disabled="disabled || isPreparingSend"
            @click="toggleFormatting"
          >
            <CaseSensitive aria-hidden="true" />
          </button>
        </AppTooltip>
        <AppTooltip label="Emoji">
          <button
            :ref="setEmojiButtonRef"
            type="button"
            :class="[iconbtn(), cls.tool]"
            aria-label="Emoji"
            aria-haspopup="dialog"
            :aria-expanded="showEmojiPicker"
            :disabled="disabled || isPreparingSend"
            @click="toggleEmojiPicker"
          >
            <Smile aria-hidden="true" />
          </button>
        </AppTooltip>
        <AppTooltip label="Mention someone">
          <button
            type="button"
            :class="[iconbtn(), cls.wide]"
            data-composer-wide
            aria-label="Mention someone"
            :disabled="disabled || isPreparingSend"
            @click="startMention"
          >
            <AtSign aria-hidden="true" />
          </button>
        </AppTooltip>
        <AppTooltip label="Run a command">
          <button
            type="button"
            :class="[iconbtn(), cls.wide]"
            data-composer-wide
            aria-label="Run a command"
            :disabled="disabled || isPreparingSend"
            @click="startSlashCommand"
          >
            <SquareSlash aria-hidden="true" />
          </button>
        </AppTooltip>
        <AppTooltip :label="isSendBusy ? 'Sending message' : 'Send now'">
          <button
            type="button"
            :class="[iconbtn({ variant: 'send' }), cls.send, !canSend && cls.sendIdle]"
            :data-armed="canSend || undefined"
            :disabled="!canSend"
            :aria-label="isSendBusy ? 'Sending message' : 'Send message'"
            :aria-busy="isSendBusy"
            @click="onSend(editorRef?.getJSON?.() ?? { type: 'doc', content: [] })"
          >
            <span v-if="slowModeCooldown > 0" :class="cls.num">{{ slowModeCooldown }}</span>
            <Loader2 v-else-if="isSendBusy" :class="spin" aria-hidden="true" />
            <SendHorizontal v-else aria-hidden="true" />
          </button>
        </AppTooltip>
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
