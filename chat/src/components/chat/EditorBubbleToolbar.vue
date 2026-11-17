<script setup lang="ts">
import type { Editor } from "@tiptap/core";
import { BubbleMenu } from "@tiptap/vue-3/menus";
import { Link } from "lucide-vue-next";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { BLOCK_FORMAT_ACTIONS, INLINE_FORMAT_ACTIONS, type EditorFormatAction } from "./editor-format-actions";
import { useEditorLinkInput } from "./composables/use-editor-link-input";
import { linkInput, toolActive } from "./composer-classes";

const props = defineProps<{
  editor: Editor;
  /** Hide the bubble while a fixed formatting bar is showing. The menu stays
   * mounted: TipTap reparents its element, so unmounting it via `v-if`
   * would leave Vue patching a detached anchor. */
  suppressed?: boolean;
}>();

const { linkUrl, editingLink, linkInputRef, openLinkInput, applyLink, cancelLinkInput } =
  useEditorLinkInput(() => props.editor);

const bar = css({
  zIndex: "dropdown",
  display: "flex",
  alignItems: "center",
  gap: "0.5",
  p: "1",
  bg: "surface",
  border: "1px solid {colors.border}",
  borderRadius: "control",
  boxShadow: "overlay",
  animation: "wd-pop {durations.base} {easings.spring.snappy} backwards",
});

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
      :class="bar"
    >
      <AppTooltip v-for="action in actions" :key="action.name" :label="action.title">
        <button
          type="button"
          :class="[iconbtn({ size: 'sm' }), action.isActive(editor) && toolActive]"
          :aria-label="action.title"
          @mousedown.prevent="runAction(action)"
        >
          <component :is="action.icon" aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip label="Link">
        <button
          type="button"
          :class="[iconbtn({ size: 'sm' }), (editingLink || editor.isActive('link')) && toolActive]"
          aria-label="Link"
          @mousedown.prevent="openLinkInput"
        >
          <Link aria-hidden="true" />
        </button>
      </AppTooltip>
      <input
        v-if="editingLink"
        ref="linkInputRef"
        v-model="linkUrl"
        type="url"
        inputmode="url"
        :class="linkInput"
        placeholder="https://example.com"
        aria-label="Link URL"
        @keydown.enter.prevent="applyLink"
        @keydown.esc.prevent="cancelLinkInput"
        @mousedown.stop
      />
    </div>
  </BubbleMenu>
</template>
