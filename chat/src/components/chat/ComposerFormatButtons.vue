<script setup lang="ts">
import { computed } from "vue";
import { Bold, Italic, Code, Link as LinkIcon } from "lucide-vue-next";
import type { Editor } from "@tiptap/core";
import { formatModShortcut } from "@/lib/composer/format-shortcut";

const props = defineProps<{
  editor: Editor | null;
  disabled: boolean;
  /**
   * Counter that bumps on every editor transaction so the parent can force
   * this component to re-evaluate `editor.isActive(...)`. Read in the
   * `items` computed solely to establish reactivity.
   */
  formatVersion: number;
  linkActive?: boolean;
}>();

const emit = defineEmits<{
  toggle: [mark: "bold" | "italic" | "code"];
  openLink: [];
}>();

const items = computed(() => {
  void props.formatVersion;
  const editor = props.editor;
  const linkOn = props.linkActive ?? editor?.isActive("link") ?? false;
  return [
    {
      name: "bold" as const,
      icon: Bold,
      title: `Bold (${formatModShortcut("Mod-B")})`,
      isActive: editor?.isActive("bold") ?? false,
      action: () => emit("toggle", "bold"),
    },
    {
      name: "italic" as const,
      icon: Italic,
      title: `Italic (${formatModShortcut("Mod-I")})`,
      isActive: editor?.isActive("italic") ?? false,
      action: () => emit("toggle", "italic"),
    },
    {
      name: "code" as const,
      icon: Code,
      title: `Inline code (${formatModShortcut("Mod-E")})`,
      isActive: editor?.isActive("code") ?? false,
      action: () => emit("toggle", "code"),
    },
    {
      name: "link" as const,
      icon: LinkIcon,
      // Tooltip flips so users know whether clicking will surface the existing
      // link's URL for editing or prompt for a new one.
      title: `${linkOn ? "Edit link" : "Add link"} (${formatModShortcut("Mod-K")})`,
      isActive: linkOn,
      action: () => emit("openLink"),
    },
  ];
});
</script>

<template>
  <div class="flex items-center gap-1">
    <button
      v-for="item in items"
      :key="item.name"
      type="button"
      class="chat-composer-input-action h-9 w-9 shrink-0 flex items-center justify-center transition-all duration-200 active:scale-[0.94] disabled:opacity-40 disabled:active:scale-100 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
      :class="
        disabled
          ? 'text-muted-foreground'
          : item.isActive
            ? 'bg-primary/10 text-primary'
            : 'text-muted-foreground hover:bg-background/70 hover:text-primary'
      "
      :title="item.title"
      :aria-label="item.title"
      :aria-pressed="item.isActive"
      :disabled="disabled"
      @mousedown.prevent
      @click="item.action"
    >
      <component :is="item.icon" class="w-4 h-4" aria-hidden="true" />
    </button>
  </div>
</template>
