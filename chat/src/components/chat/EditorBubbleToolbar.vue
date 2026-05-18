<script setup lang="ts">
import { computed } from "vue";
import type { Editor } from "@tiptap/core";
import { BubbleMenu } from "@tiptap/vue-3/menus";
import { Strikethrough, List, ListOrdered, TextQuote, SquareCode } from "lucide-vue-next";
import { formatModShortcut } from "@/lib/composer/format-shortcut";

const props = defineProps<{
  editor: Editor;
}>();

type ToolbarAction =
  | "strike"
  | "bulletList"
  | "orderedList"
  | "blockquote"
  | "codeBlock";

function runCommand(action: ToolbarAction) {
  const chain = props.editor.chain().focus();
  switch (action) {
    case "strike":
      chain.toggleStrike().run();
      break;
    case "bulletList":
      chain.toggleBulletList().run();
      break;
    case "orderedList":
      chain.toggleOrderedList().run();
      break;
    case "blockquote":
      chain.toggleBlockquote().run();
      break;
    case "codeBlock":
      chain.toggleCodeBlock().run();
      break;
  }
}

const items = computed(() => [
  {
    name: "strike",
    icon: Strikethrough,
    title: `Strikethrough (${formatModShortcut("Mod-Shift-X")})`,
    action: () => runCommand("strike"),
    isActive: () => props.editor.isActive("strike"),
  },
  {
    name: "bullet-list",
    icon: List,
    title: `Bullet list (${formatModShortcut("Mod-Shift-8")})`,
    action: () => runCommand("bulletList"),
    isActive: () => props.editor.isActive("bulletList"),
  },
  {
    name: "ordered-list",
    icon: ListOrdered,
    title: `Numbered list (${formatModShortcut("Mod-Shift-7")})`,
    action: () => runCommand("orderedList"),
    isActive: () => props.editor.isActive("orderedList"),
  },
  {
    name: "blockquote",
    icon: TextQuote,
    title: `Quote (${formatModShortcut("Mod-Shift-B")})`,
    action: () => runCommand("blockquote"),
    isActive: () => props.editor.isActive("blockquote"),
  },
  {
    name: "code-block",
    icon: SquareCode,
    title: `Code block (${formatModShortcut("Mod-Alt-C")})`,
    action: () => runCommand("codeBlock"),
    isActive: () => props.editor.isActive("codeBlock"),
  },
]);
</script>

<template>
  <BubbleMenu :editor="editor">
    <div
      class="z-popover flex items-center gap-1.5 p-1.5 glass-panel border border-border rounded-lg shadow-xl animate-fade-in"
    >
      <button
        v-for="item in items"
        :key="item.name"
        type="button"
        class="type-control h-8 w-8 flex items-center justify-center rounded-md transition-all duration-150"
        :class="
          item.isActive?.()
            ? 'bg-primary/10 text-primary'
            : 'text-muted-foreground hover:bg-muted hover:text-foreground'
        "
        :title="item.title"
        :aria-label="item.title"
        :aria-pressed="item.isActive?.() ?? false"
        @mousedown.prevent="item.action"
      >
        <component :is="item.icon" class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
    </div>
  </BubbleMenu>
</template>
