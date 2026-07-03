<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { Loader2, X } from "lucide-vue-next";
import type { JSONContent } from "@tiptap/core";
import ChatEditor from "@/components/chat/ChatEditor.vue";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";
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
  <div class="chat-message-fill flex min-w-0 items-start gap-1.5">
    <div class="flex min-w-0 flex-1 flex-col gap-1.5">
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
        class="flex min-w-0 items-center gap-2 rounded-md border border-border bg-muted/45 px-2 py-1.5"
        :aria-busy="editLinkPreview.state.value.kind === 'loading'"
      >
        <Loader2
          v-if="editLinkPreview.state.value.kind === 'loading'"
          class="h-4 w-4 shrink-0 animate-spin text-primary"
          aria-hidden="true"
        />
        <div class="min-w-0 flex-1">
          <div class="type-emphasis truncate text-foreground">{{ editLinkPreview.title.value }}</div>
          <div class="type-caption truncate text-muted-foreground">{{ editLinkPreview.description.value }}</div>
        </div>
        <button
          v-if="editLinkPreview.canDismiss.value"
          type="button"
          class="chat-composer-input-action h-7 w-7 shrink-0 flex items-center justify-center text-muted-foreground transition-colors hover:bg-background/70 hover:text-foreground"
          aria-label="Remove preview"
          @click="editLinkPreview.dismiss"
        >
          <X class="h-4 w-4" aria-hidden="true" />
        </button>
      </div>
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
</template>
