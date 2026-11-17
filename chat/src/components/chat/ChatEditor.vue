<script setup lang="ts">
import { watch, onBeforeUnmount } from "vue";
import { useEditor, EditorContent } from "@tiptap/vue-3";
import type { JSONContent } from "@tiptap/core";
import { css } from "styled-system/css";
import { createChatEditorExtensions } from "@/lib/editor/chat-editor-extensions";

const props = withDefaults(
  defineProps<{
    placeholder?: string;
    disabled?: boolean;
    compact?: boolean;
    embedded?: boolean;
    initialContent?: JSONContent;
    editorLabel?: string;
    /**
     * Synchronous paste decision. Return `true` when the paste was
     * handled so ProseMirror inserts nothing; `false` for a normal paste.
     */
    pasteHandler?: (event: ClipboardEvent) => boolean;
  }>(),
  {
    placeholder: "Type a message…",
    disabled: false,
    compact: false,
    embedded: false,
  },
);

const emit = defineEmits<{
  send: [doc: JSONContent];
  typing: [];
  cancel: [];
  update: [doc: JSONContent];
  selectionUpdate: [];
}>();

const editor = useEditor({
  enableCoreExtensions: { keymap: false },
  extensions: createChatEditorExtensions({
    placeholder: () => props.placeholder,
    onSubmit: (doc) => {
      emit("send", doc);
      // Don't clear here — let the parent call clear() after a successful send
      // to avoid losing content on send failures.
      return true;
    },
  }),
  immediatelyRender: false,
  editable: !props.disabled,
  content: props.initialContent ?? "",
  editorProps: {
    handleKeyDown: (_view, event) => {
      if (event.key === "Escape") {
        emit("cancel");
        return true;
      }
      return false;
    },
    handlePaste: (_view, event) => props.pasteHandler?.(event) ?? false,
    attributes: {
      ...(props.editorLabel ? { "aria-label": props.editorLabel } : {}),
      class: "outline-none",
    },
  },
  onUpdate: ({ editor: ed }) => {
    emit("typing");
    emit("update", ed.getJSON());
  },
  onSelectionUpdate: () => {
    emit("selectionUpdate");
  },
});

watch(
  () => props.disabled,
  (disabled) => {
    editor.value?.setEditable(!disabled);
  },
);

watch(
  () => props.initialContent,
  (content) => {
    if (content && editor.value) {
      editor.value.commands.setContent(content);
    }
  },
);

onBeforeUnmount(() => {
  editor.value?.destroy();
});

defineExpose({
  focus: () => editor.value?.commands.focus(),
  clear: () => editor.value?.commands.clearContent(),
  isEmpty: () => editor.value?.isEmpty ?? true,
  getJSON: () => editor.value?.getJSON(),
  editor: editor,
});

const root = css({
  display: "flex",
  alignItems: "center",
  minWidth: 0,
  cursor: "text",
  color: "fg",
  font: "400 16px/1.5 {fonts.body}",
  "& .ProseMirror": {
    outline: "none",
    minWidth: 0,
    width: "100%",
    minHeight: "24px",
    maxHeight: "160px",
    overflowY: "auto",
    overflowWrap: "anywhere",
    scrollbarWidth: "thin",
  },
  "& .ProseMirror p.is-editor-empty:first-child::before": {
    content: "attr(data-placeholder)",
    color: "fg.soft",
    float: "left",
    height: 0,
  },
});
// Inside the composer field: the field draws the frame.
const embeddedClass = css({ flex: 1, minHeight: "40px", px: "2", py: "2" });
const compactClass = css({ minHeight: "36px", py: "1" });
// Standalone (the inline edit form): its own field.
const fullClass = css({ flex: 1, minHeight: "40px", py: "1.5" });
const framedClass = css({
  px: "3",
  borderRadius: "control",
  border: "1px solid {colors.border.strong}",
  bg: "surface",
  _focusWithin: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
});
const disabledClass = css({ opacity: 0.4, pointerEvents: "none" });
const contentClass = css({ width: "100%", minWidth: 0 });
</script>

<template>
  <!-- `chat-editor` is the hook the shared prose styles (rich-text.css) use. -->
  <div
    role="textbox"
    :aria-label="editorLabel"
    :class="[
      'chat-editor',
      root,
      embedded ? embeddedClass : [framedClass, compact ? compactClass : fullClass],
      disabled && disabledClass,
    ]"
    @click="editor?.commands.focus()"
  >
    <EditorContent
      v-if="editor"
      :editor="editor"
      :class="contentClass"
    />
  </div>
</template>
