<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { Loader2, X } from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
import { spin } from "./composer-classes";
import type { LinkPreview, MarkupSpan, MessageReference } from "@/lib/chat-ui";
import { richMessageToTiptap, tiptapToRichMessage } from "@/lib/rich-message";
import { useComposerLinkPreview } from "@/lib/use-composer-link-preview";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";

const props = defineProps<{
  body: string;
  markup?: MarkupSpan[];
  references?: MessageReference[];
  linkPreviews?: LinkPreview[];
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
}>();

const emit = defineEmits<{
  save: [newBody: string, markup?: MarkupSpan[], references?: MessageReference[], linkPreview?: ComposerLinkPreviewSendPayload];
  close: [];
}>();

const form = css({ gridColumn: 2, width: "100%", alignSelf: "stretch", display: "flex", minWidth: 0, alignItems: "flex-start", gap: "1.5", mt: "1" });
const stack = css({ display: "flex", minWidth: 0, flex: 1, flexDirection: "column", gap: "1.5" });
const previewCard = css({ display: "flex", minWidth: 0, alignItems: "center", gap: "2", px: "2", py: "1.5", borderRadius: "sm", border: "1px solid {colors.border}", bg: "surface.2" });
const spinner = css({ width: "16px", height: "16px", flex: "none", color: "action.text" });
const previewText = css({ minWidth: 0, flex: 1, "& > div": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" } });
const previewTitle = css({ fontWeight: 700, fontSize: "sm", color: "fg" });
const previewDesc = css({ textStyle: "caption", color: "fg.muted" });
const hint = css({ textStyle: "caption", color: "fg.muted" });
const hintAction = css({ border: 0, bg: "transparent", p: 0, font: "inherit", fontWeight: 700, color: "action.text", cursor: "pointer", _hover: { textDecoration: "underline" } });
const dot = css({ mx: "1", color: "fg.soft" });

const editInitialContent = richMessageToTiptap({
  body: props.body,
  markup: props.markup,
  references: props.references,
});

const editEditorRef = ref<InstanceType<typeof ChatEditor> | null>(null);
const editDraft = ref(props.body);
const isSubmittingEdit = ref(false);
const closed = ref(false);
const setEditEditorRef = (instance: InstanceType<typeof ChatEditor> | null) => {
  editEditorRef.value = instance;
};
const editTiptapEditor = computed(() => {
  const e = editEditorRef.value as any;
  return e?.editor?.value ?? e?.editor ?? null;
});
const editOriginalRich = computed(() =>
  tiptapToRichMessage(richMessageToTiptap({
    body: props.body,
    markup: props.markup,
    references: props.references,
  })),
);
const editOriginalBody = computed(() => editOriginalRich.value.body.trim());
const editOriginalHasPreview = computed(() => (props.linkPreviews?.length ?? 0) > 0);
const editOriginalPreviewUrl = computed(() => props.linkPreviews?.[0]?.originalUrl ?? null);
const editLinkPreview = useComposerLinkPreview(
  editDraft,
  computed(() => props.linkPreviewLookup),
  computed(() => props.linkPreviewScope),
);

onMounted(() => {
  editEditorRef.value?.focus();
});

onBeforeUnmount(() => {
  closed.value = true;
});

function cancelEdit() {
  closed.value = true;
  emit("close");
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
    if (closed.value || editDraft.value !== draftAtSubmit) return;
  }
  const previewChanged = originalHasPreview
    ? previewDismissed
      || originalPreviewUrlRemoved
      || (!!linkPreview && linkPreview.preview.originalUrl !== originalPreviewUrl)
    : !!linkPreview;
  const changed = contentChanged || previewChanged;
  if (trimmed && changed) {
    emit("save", body, markup, references, linkPreview);
  }
  closed.value = true;
  emit("close");
}

function submitEditFromLink() {
  if (isSubmittingEdit.value) return;
  const doc = editEditorRef.value?.getJSON();
  if (!doc) return;
  void submitEditFromEditor(doc);
}
</script>

<template>
  <div :class="form">
    <div :class="stack">
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
        :class="previewCard"
        :aria-busy="editLinkPreview.state.value.kind === 'loading'"
      >
        <Loader2
          v-if="editLinkPreview.state.value.kind === 'loading'"
          :class="[spinner, spin]"
          aria-hidden="true"
        />
        <div :class="previewText">
          <div :class="previewTitle">{{ editLinkPreview.title.value }}</div>
          <div :class="previewDesc">{{ editLinkPreview.description.value }}</div>
        </div>
        <button
          v-if="editLinkPreview.canDismiss.value"
          type="button"
          :class="iconbtn({ size: 'sm' })"
          aria-label="Remove preview"
          @click="editLinkPreview.dismiss"
        >
          <X aria-hidden="true" />
        </button>
      </div>
      <p :class="hint">
        escape to
        <button
          type="button"
          :class="hintAction"
          @click="cancelEdit"
        >
          cancel
        </button>
        <span :class="dot">•</span>
        <button
          type="button"
          :class="hintAction"
          @click="submitEditFromLink"
        >
          enter
        </button>
        to save
      </p>
    </div>
    <EditorBubbleToolbar v-if="editTiptapEditor" :editor="editTiptapEditor" />
  </div>
</template>
