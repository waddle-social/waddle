<script setup lang="ts">
import { watch, onBeforeUnmount } from "vue";
import { useEditor, EditorContent } from "@tiptap/vue-3";
import type { JSONContent } from "@tiptap/core";
import { createChatEditorExtensions } from "@/lib/editor/chat-editor-extensions";

const props = withDefaults(
  defineProps<{
    placeholder?: string;
    disabled?: boolean;
    compact?: boolean;
    embedded?: boolean;
    initialContent?: JSONContent;
    editorLabel?: string;
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
  paste: [event: ClipboardEvent];
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
    handlePaste: (_view, event) => {
      emit("paste", event);
      return false;
    },
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
</script>

<template>
  <div
    class="chat-editor type-chat-editor flex min-w-0 items-center transition-all duration-300 cursor-text"
    :class="[
      embedded ? 'min-h-9 flex-1 px-2 py-1' : compact ? 'min-h-9 py-1' : 'min-h-10 flex-1 py-1.5',
      embedded ? '' : 'rounded-lg bg-muted px-4 has-[:focus]:ring-2 has-[:focus]:ring-primary/20 has-[:focus]:shadow-[0_0_16px_var(--glow)]',
      disabled ? 'opacity-40 pointer-events-none' : '',
    ]"
    @click="editor?.commands.focus()"
  >
    <EditorContent
      v-if="editor"
      :editor="editor"
      class="w-full min-w-0"
    />
  </div>
</template>

<!--
  Minimal ProseMirror styles that cannot be expressed as Tailwind utility classes
  (pseudo-element placeholder, nested prose elements).
-->
<style>
.chat-editor .ProseMirror {
  outline: none;
  min-width: 0;
  width: 100%;
  min-height: 1.25rem;
  max-height: 10rem;
  line-height: var(--leading-message);
  overflow-y: auto;
  overflow-wrap: anywhere;
}

/* Placeholder */
.chat-editor .ProseMirror p.is-editor-empty:first-child::before {
  content: attr(data-placeholder);
  color: var(--muted-foreground);
  opacity: 0.4;
  float: left;
  height: 0;
  pointer-events: none;
}

/* Compact mode — tighter max-height */
.chat-editor:has(.ProseMirror) .ProseMirror {
  scrollbar-width: thin;
}
</style>
