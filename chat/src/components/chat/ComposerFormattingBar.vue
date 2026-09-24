<script setup lang="ts">
import type { Editor } from "@tiptap/core";
import { Link } from "lucide-vue-next";
import { BLOCK_FORMAT_ACTIONS, INLINE_FORMAT_ACTIONS, type EditorFormatAction } from "./editor-format-actions";
import { useEditorLinkInput } from "./composables/use-editor-link-input";

/**
 * Slack-style fixed formatting row pinned to the top of the composer card,
 * toggled by the composer's `Aa` button. Buttons act on `mousedown` so the
 * editor keeps its selection and focus.
 */
const props = defineProps<{
  editor: Editor;
  disabled?: boolean;
}>();

const { linkUrl, editingLink, linkInputRef, openLinkInput, applyLink, cancelLinkInput } =
  useEditorLinkInput(() => props.editor);

const groups: readonly (readonly EditorFormatAction[])[] = [INLINE_FORMAT_ACTIONS, BLOCK_FORMAT_ACTIONS];

function runAction(action: EditorFormatAction) {
  if (props.disabled) return;
  editingLink.value = false;
  action.run(props.editor);
}

function onLinkClick() {
  if (props.disabled) return;
  openLinkInput();
}
</script>

<template>
  <div class="chat-composer-format-bar" role="toolbar" aria-label="Formatting">
    <template v-for="(group, groupIndex) in groups" :key="groupIndex">
      <span v-if="groupIndex > 0" class="chat-composer-format-bar__divider" aria-hidden="true" />
      <button
        v-for="action in group"
        :key="action.name"
        type="button"
        class="chat-composer-tool"
        :class="{ 'chat-composer-tool--active': action.isActive(editor) }"
        :title="action.title"
        :aria-label="action.title"
        :aria-pressed="action.isActive(editor)"
        :disabled="disabled"
        @mousedown.prevent
        @click="runAction(action)"
      >
        <component :is="action.icon" class="h-4 w-4" aria-hidden="true" />
      </button>
      <button
        v-if="groupIndex === 0"
        type="button"
        class="chat-composer-tool"
        :class="{ 'chat-composer-tool--active': editingLink || editor.isActive('link') }"
        title="Link"
        aria-label="Link"
        :aria-pressed="editor.isActive('link')"
        :disabled="disabled"
        @mousedown.prevent
        @click="onLinkClick"
      >
        <Link class="h-4 w-4" aria-hidden="true" />
      </button>
    </template>
    <input
      v-if="editingLink"
      ref="linkInputRef"
      v-model="linkUrl"
      type="url"
      inputmode="url"
      class="type-caption ml-1 h-7 w-48 min-w-0 rounded-md border border-border bg-background px-2 text-foreground outline-none focus:border-primary"
      placeholder="https://example.com"
      aria-label="Link URL"
      @keydown.enter.prevent="applyLink"
      @keydown.esc.prevent.stop="cancelLinkInput"
    />
  </div>
</template>
