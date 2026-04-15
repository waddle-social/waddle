<script setup lang="ts">
import { computed, ref, onMounted, onBeforeUnmount, watchEffect } from "vue";
import type { Editor } from "@tiptap/vue-3";
import { BubbleMenuPlugin } from "@tiptap/extension-bubble-menu";
import { Bold, Italic, Strikethrough, Code, Link } from "lucide-vue-next";

const props = defineProps<{
  editor: Editor;
}>();

const menuRef = ref<HTMLDivElement | null>(null);
const isVisible = ref(false);

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

let pluginInstance: ReturnType<typeof BubbleMenuPlugin> | null = null;

onMounted(() => {
  if (!menuRef.value || !props.editor) return;

  pluginInstance = BubbleMenuPlugin({
    pluginKey: "waddleBubbleMenu",
    editor: props.editor,
    element: menuRef.value,
    updateDelay: 150,
    shouldShow: ({ editor, state }) => {
      const { selection } = state;
      const { empty } = selection;
      if (empty) {
        isVisible.value = false;
        return false;
      }
      isVisible.value = true;
      return true;
    },
  });

  props.editor.registerPlugin(pluginInstance);
});

onBeforeUnmount(() => {
  if (pluginInstance && props.editor) {
    props.editor.unregisterPlugin("waddleBubbleMenu");
  }
});
</script>

<template>
  <div
    ref="menuRef"
    :style="{ visibility: isVisible ? 'visible' : 'hidden' }"
  >
    <div
      class="flex items-center gap-0.5 px-1.5 py-1 glass-panel border border-border rounded-lg shadow-xl animate-fade-in"
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
  </div>
</template>
