<script setup lang="ts">
import { watch, onBeforeUnmount, computed } from "vue";
import { useEditor, EditorContent } from "@tiptap/vue-3";
import StarterKit from "@tiptap/starter-kit";
import Link from "@tiptap/extension-link";
import Image from "@tiptap/extension-image";
import Placeholder from "@tiptap/extension-placeholder";
import EditorBubbleToolbar from "@/components/chat/EditorBubbleToolbar.vue";

const props = withDefaults(
  defineProps<{
    placeholder?: string;
    disabled?: boolean;
    compact?: boolean;
    initialContent?: Record<string, unknown>;
  }>(),
  {
    placeholder: "Type a message...",
    disabled: false,
    compact: false,
  },
);

const emit = defineEmits<{
  send: [doc: Record<string, unknown>];
  typing: [];
  cancel: [];
  update: [doc: Record<string, unknown>];
}>();

const extensions = computed(() => {
  const exts = [
    StarterKit.configure({
      heading: false,
      horizontalRule: false,
      link: false,
    }),
    Link.configure({
      openOnClick: false,
      autolink: true,
      HTMLAttributes: {
        class: "text-primary underline decoration-primary/40 hover:decoration-primary transition-colors",
      },
    }),
    Placeholder.configure({
      placeholder: props.placeholder,
    }),
  ];

  if (!props.compact) {
    exts.push(
      Image.configure({
        inline: false,
        allowBase64: false,
      }),
    );
  }

  return exts;
});

function handleSend() {
  if (!editor.value || editor.value.isEmpty) return;
  emit("send", editor.value.getJSON());
  // Don't clear here — let the parent call clear() after a successful send
  // to avoid losing content on send failures.
}

const editor = useEditor({
  extensions: extensions.value,
  immediatelyRender: false,
  editable: !props.disabled,
  content: props.initialContent ?? "",
  editorProps: {
    handleKeyDown: (_view, event) => {
      if (event.key === "Enter" && !event.shiftKey && !event.ctrlKey && !event.metaKey) {
        event.preventDefault();
        handleSend();
        return true;
      }
      if (event.key === "Escape") {
        emit("cancel");
        return true;
      }
      return false;
    },
    attributes: {
      class: "outline-none",
    },
  },
  onUpdate: ({ editor: ed }) => {
    emit("typing");
    emit("update", ed.getJSON());
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
  <div class="relative" :class="compact ? '' : 'flex-1'">
    <div
      class="chat-editor rounded-xl bg-muted text-[13px] px-4 transition-all duration-300 has-[:focus]:ring-2 has-[:focus]:ring-primary/20 has-[:focus]:shadow-[0_0_16px_var(--glow)]"
      :class="[
        compact ? 'min-h-9' : 'min-h-10',
        disabled ? 'opacity-40 pointer-events-none' : '',
      ]"
    >
      <EditorContent
        v-if="editor"
        :editor="editor"
        class="flex items-center"
        :class="compact ? 'py-1.5' : 'py-2.5'"
      />
    </div>
    <EditorBubbleToolbar v-if="editor && !compact" :editor="editor" />
  </div>
</template>

<!--
  Minimal ProseMirror styles that cannot be expressed as Tailwind utility classes
  (pseudo-element placeholder, nested prose elements).
-->
<style>
.chat-editor .ProseMirror {
  outline: none;
  min-height: 1.25rem;
  max-height: 10rem;
  overflow-y: auto;
}

.chat-editor .ProseMirror p {
  margin: 0;
}

.chat-editor .ProseMirror p + p {
  margin-top: 0.25em;
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

/* Inline code */
.chat-editor .ProseMirror code {
  font-family: "JetBrains Mono Variable", ui-monospace, monospace;
  font-size: 0.8em;
  padding: 0.15em 0.4em;
  background: var(--muted);
  border-radius: 5px;
}

/* Code blocks */
.chat-editor .ProseMirror pre {
  font-family: "JetBrains Mono Variable", ui-monospace, monospace;
  font-size: 0.8em;
  padding: 0.85em 1.1em;
  background: var(--muted);
  border-radius: 8px;
  overflow-x: auto;
  margin: 0.5em 0;
}

/* Blockquote */
.chat-editor .ProseMirror blockquote {
  border-left: 2px solid var(--primary);
  padding-left: 0.85em;
  margin: 0.5em 0;
  opacity: 0.75;
}

/* Lists */
.chat-editor .ProseMirror ul {
  list-style-type: disc;
  padding-left: 1.25em;
  margin: 0.25em 0;
}

.chat-editor .ProseMirror ol {
  list-style-type: decimal;
  padding-left: 1.25em;
  margin: 0.25em 0;
}

.chat-editor .ProseMirror li {
  margin: 0.1em 0;
}

/* Images */
.chat-editor .ProseMirror img {
  max-width: 100%;
  max-height: 14rem;
  border-radius: 8px;
  border: 1px solid var(--border);
  margin: 0.5em 0;
}

/* Links */
.chat-editor .ProseMirror a {
  color: var(--primary);
  text-underline-offset: 2px;
}

/* Compact mode — tighter max-height */
.chat-editor:has(.ProseMirror) .ProseMirror {
  scrollbar-width: thin;
}
</style>
