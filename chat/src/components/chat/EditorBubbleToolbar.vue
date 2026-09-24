<script setup lang="ts">
import type { Editor } from "@tiptap/core";
import { BubbleMenu } from "@tiptap/vue-3/menus";
import { Link } from "lucide-vue-next";
import { BLOCK_FORMAT_ACTIONS, INLINE_FORMAT_ACTIONS, type EditorFormatAction } from "./editor-format-actions";
import { useEditorLinkInput } from "./composables/use-editor-link-input";

const props = defineProps<{
  editor: Editor;
  /** Hide the bubble while a fixed formatting bar is showing. The menu stays
   * mounted: TipTap reparents its element, so unmounting it via `v-if`
   * would leave Vue patching a detached anchor. */
  suppressed?: boolean;
}>();

const { linkUrl, editingLink, linkInputRef, openLinkInput, applyLink, cancelLinkInput } =
  useEditorLinkInput(() => props.editor);

const actions: readonly EditorFormatAction[] = [...INLINE_FORMAT_ACTIONS, ...BLOCK_FORMAT_ACTIONS];

function runAction(action: EditorFormatAction) {
  action.run(props.editor);
  editingLink.value = false;
}
</script>

<template>
  <BubbleMenu :editor="editor">
    <div
      v-show="!suppressed"
      class="z-popover flex items-center gap-1.5 p-1.5 glass-panel border border-border rounded-lg shadow-xl animate-fade-in"
    >
      <button
        v-for="action in actions"
        :key="action.name"
        type="button"
        class="type-control h-8 w-8 flex items-center justify-center rounded-md transition-all duration-150"
        :class="
          action.isActive(editor)
            ? 'bg-primary/10 text-primary'
            : 'text-muted-foreground hover:bg-muted hover:text-foreground'
        "
        :title="action.title"
        :aria-label="action.title"
        @mousedown.prevent="runAction(action)"
      >
        <component :is="action.icon" class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="type-control h-8 w-8 flex items-center justify-center rounded-md transition-all duration-150"
        :class="
          editingLink || editor.isActive('link')
            ? 'bg-primary/10 text-primary'
            : 'text-muted-foreground hover:bg-muted hover:text-foreground'
        "
        title="Link"
        aria-label="Link"
        @mousedown.prevent="openLinkInput"
      >
        <Link class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
      <input
        v-if="editingLink"
        ref="linkInputRef"
        v-model="linkUrl"
        type="url"
        inputmode="url"
        class="type-caption h-8 w-48 rounded-md border border-border bg-background px-2 text-foreground outline-none focus:border-primary"
        placeholder="https://example.com"
        aria-label="Link URL"
        @keydown.enter.prevent="applyLink"
        @keydown.esc.prevent="cancelLinkInput"
        @mousedown.stop
      />
    </div>
  </BubbleMenu>
</template>
