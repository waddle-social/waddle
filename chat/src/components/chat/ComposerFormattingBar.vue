<script setup lang="ts">
import type { Editor } from "@tiptap/core";
import { Link } from "lucide-vue-next";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { BLOCK_FORMAT_ACTIONS, INLINE_FORMAT_ACTIONS, type EditorFormatAction } from "./editor-format-actions";
import { useEditorLinkInput } from "./composables/use-editor-link-input";
import { linkInput, toolActive } from "./composer-classes";

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

// Spans the composer field's full width above the editor row.
const bar = css({
  order: -2,
  flexBasis: "100%",
  display: "flex",
  flexWrap: "wrap",
  alignItems: "center",
  gap: "0.5",
  pb: "1",
  mb: "0.5",
  borderBottom: "1px solid {colors.border.soft}",
});
const divider = css({ width: "1px", height: "20px", mx: "1", bg: "border.soft" });

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
  <div :class="bar" role="toolbar" aria-label="Formatting">
    <template v-for="(group, groupIndex) in groups" :key="groupIndex">
      <span v-if="groupIndex > 0" :class="divider" aria-hidden="true" />
      <AppTooltip v-for="action in group" :key="action.name" :label="action.title">
        <button
          type="button"
          :class="iconbtn({ size: 'sm' })"
          :aria-label="action.title"
          :aria-pressed="action.isActive(editor)"
          :disabled="disabled"
          @mousedown.prevent
          @click="runAction(action)"
        >
          <component :is="action.icon" aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip v-if="groupIndex === 0" label="Link">
        <button
          type="button"
          :class="[iconbtn({ size: 'sm' }), editingLink && toolActive]"
          aria-label="Link"
          :aria-pressed="editor.isActive('link')"
          :disabled="disabled"
          @mousedown.prevent
          @click="onLinkClick"
        >
          <Link aria-hidden="true" />
        </button>
      </AppTooltip>
    </template>
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
      @keydown.esc.prevent.stop="cancelLinkInput"
    />
  </div>
</template>
