<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import type { Editor } from "@tiptap/core";
import { BubbleMenu } from "@tiptap/vue-3/menus";
import { Bold, Italic, Strikethrough, Code, Link, List, ListOrdered, TextQuote, SquareCode } from "lucide-vue-next";

const props = defineProps<{
  editor: Editor;
}>();

const linkUrl = ref("");
const editingLink = ref(false);
const linkInputRef = ref<HTMLInputElement | null>(null);

type ToolbarAction =
  | "bold"
  | "italic"
  | "strike"
  | "code"
  | "bulletList"
  | "orderedList"
  | "blockquote"
  | "codeBlock";

function runCommand(action: ToolbarAction) {
  const chain = props.editor.chain().focus();
  switch (action) {
    case "bold":
      chain.toggleBold().run();
      break;
    case "italic":
      chain.toggleItalic().run();
      break;
    case "strike":
      chain.toggleStrike().run();
      break;
    case "code":
      chain.toggleCode().run();
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
  editingLink.value = false;
}

function openLinkInput() {
  editingLink.value = true;
  const href = props.editor.getAttributes("link").href;
  linkUrl.value = typeof href === "string" && href ? href : "https://";
  void nextTick(() => linkInputRef.value?.focus());
}

function applyLink() {
  const href = sanitizeLinkUrl(linkUrl.value);
  if (href) {
    props.editor.chain().focus().extendMarkRange("link").setLink({ href }).run();
  } else {
    props.editor.chain().focus().extendMarkRange("link").unsetLink().run();
  }
  editingLink.value = false;
}

function sanitizeLinkUrl(url: string): string | null {
  const trimmed = url.trim();
  if (!trimmed) return null;
  try {
    const parsed = new URL(trimmed);
    if (!["http:", "https:", "mailto:"].includes(parsed.protocol)) return null;
    return parsed.toString();
  } catch {
    return null;
  }
}

const items = computed(() => [
  {
    name: "bold",
    icon: Bold,
    title: "Bold",
    action: () => runCommand("bold"),
    isActive: () => props.editor.isActive("bold"),
  },
  {
    name: "italic",
    icon: Italic,
    title: "Italic",
    action: () => runCommand("italic"),
    isActive: () => props.editor.isActive("italic"),
  },
  {
    name: "strike",
    icon: Strikethrough,
    title: "Strikethrough",
    action: () => runCommand("strike"),
    isActive: () => props.editor.isActive("strike"),
  },
  {
    name: "code",
    icon: Code,
    title: "Inline code",
    action: () => runCommand("code"),
    isActive: () => props.editor.isActive("code"),
  },
  {
    name: "bullet-list",
    icon: List,
    title: "Bullet list",
    action: () => runCommand("bulletList"),
    isActive: () => props.editor.isActive("bulletList"),
  },
  {
    name: "ordered-list",
    icon: ListOrdered,
    title: "Numbered list",
    action: () => runCommand("orderedList"),
    isActive: () => props.editor.isActive("orderedList"),
  },
  {
    name: "blockquote",
    icon: TextQuote,
    title: "Quote",
    action: () => runCommand("blockquote"),
    isActive: () => props.editor.isActive("blockquote"),
  },
  {
    name: "code-block",
    icon: SquareCode,
    title: "Code block",
    action: () => runCommand("codeBlock"),
    isActive: () => props.editor.isActive("codeBlock"),
  },
  {
    name: "link",
    icon: Link,
    title: "Link",
    action: openLinkInput,
    isActive: () => props.editor.isActive("link"),
  },
]);
</script>

<template>
  <BubbleMenu :editor="editor">
    <div
      class="z-50 flex items-center gap-1.5 p-1.5 glass-panel border border-border rounded-lg shadow-xl animate-fade-in"
    >
      <button
        v-for="item in items"
        :key="item.name"
        class="h-8 w-8 flex items-center justify-center rounded-md transition-all duration-150 text-[13px]"
        :class="
          (editingLink && item.name === 'link') || item.isActive?.()
            ? 'bg-primary/10 text-primary'
            : 'text-muted-foreground hover:bg-muted hover:text-foreground'
        "
        :title="item.title"
        @mousedown.prevent="item.action"
      >
        <component :is="item.icon" class="w-3.5 h-3.5" />
      </button>
      <input
        v-if="editingLink"
        ref="linkInputRef"
        v-model="linkUrl"
        type="url"
        inputmode="url"
        class="h-8 w-48 rounded-md border border-border bg-background px-2 text-[12px] text-foreground outline-none focus:border-primary"
        placeholder="https://example.com"
        @keydown.enter.prevent="applyLink"
        @keydown.esc.prevent="editingLink = false"
        @mousedown.stop
      />
    </div>
  </BubbleMenu>
</template>
