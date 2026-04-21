<script setup lang="ts">
import { computed } from "vue";
import type { Editor } from "@tiptap/vue-3";
import { BubbleMenu } from "@tiptap/vue-3/menus";
import { Bold, Italic, Strikethrough, Code, Link, List, ListOrdered, TextQuote, SquareCode } from "lucide-vue-next";

const props = defineProps<{
  editor: Editor;
}>();

const items = computed(() => [
  {
    name: "bold",
    icon: Bold,
    title: "Bold (⌘B)",
    action: () => props.editor.chain().focus().toggleBold().run(),
    isActive: () => props.editor.isActive("bold"),
  },
  {
    name: "italic",
    icon: Italic,
    title: "Italic (⌘I)",
    action: () => props.editor.chain().focus().toggleItalic().run(),
    isActive: () => props.editor.isActive("italic"),
  },
  {
    name: "strike",
    icon: Strikethrough,
    title: "Strikethrough (⌘⇧X)",
    action: () => props.editor.chain().focus().toggleStrike().run(),
    isActive: () => props.editor.isActive("strike"),
  },
  {
    name: "code",
    icon: Code,
    title: "Code (⌘E)",
    action: () => props.editor.chain().focus().toggleCode().run(),
    isActive: () => props.editor.isActive("code"),
  },
  {
    name: "bullet-list",
    icon: List,
    title: "Bullet list",
    action: () => props.editor.chain().focus().toggleBulletList().run(),
    isActive: () => props.editor.isActive("bulletList"),
  },
  {
    name: "ordered-list",
    icon: ListOrdered,
    title: "Numbered list",
    action: () => props.editor.chain().focus().toggleOrderedList().run(),
    isActive: () => props.editor.isActive("orderedList"),
  },
  {
    name: "blockquote",
    icon: TextQuote,
    title: "Quote",
    action: () => props.editor.chain().focus().toggleBlockquote().run(),
    isActive: () => props.editor.isActive("blockquote"),
  },
  {
    name: "code-block",
    icon: SquareCode,
    title: "Code block",
    action: () => props.editor.chain().focus().toggleCodeBlock().run(),
    isActive: () => props.editor.isActive("codeBlock"),
  },
  {
    name: "link",
    icon: Link,
    title: "Link (⌘K)",
    action: () => {
      if (props.editor.isActive("link")) {
        props.editor.chain().focus().unsetLink().run();
        return;
      }
      const url = window.prompt("URL:");
      if (url) {
        props.editor.chain().focus().setLink({ href: url }).run();
      }
    },
    isActive: () => props.editor.isActive("link"),
  },
]);
</script>

<template>
  <BubbleMenu :editor="editor">
    <div
      class="z-50 flex items-center gap-0.5 px-1.5 py-1 glass-panel border border-border rounded-lg shadow-xl animate-fade-in"
    >
      <button
        v-for="item in items"
        :key="item.name"
        class="h-7 w-7 flex items-center justify-center rounded-md transition-all duration-150 text-[13px]"
        :class="
          item.isActive?.()
            ? 'bg-primary/10 text-primary'
            : 'text-muted-foreground hover:bg-muted hover:text-foreground'
        "
        :title="item.title"
        @mousedown.prevent="item.action"
      >
        <component :is="item.icon" class="w-3.5 h-3.5" />
      </button>
    </div>
  </BubbleMenu>
</template>
