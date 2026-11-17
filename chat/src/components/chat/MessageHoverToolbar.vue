<script setup lang="ts">
import { ref } from "vue";
import { MessageSquare, Pencil, Pin, PinOff, Reply, SmilePlus, Trash2 } from "lucide-vue-next";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";
import { css, cx } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";

defineProps<{
  pickerOpen: boolean;
  /** Hover/focus/lock-driven visibility classes computed by the owner of
   * the desktop-toolbar lock (the message card). */
  visibilityClass: string;
  reactionModeSelected: boolean;
  canReply: boolean;
  threadReplyCount: number;
  canPin: boolean;
  isPinned: boolean;
  isSelf: boolean;
}>();

const emit = defineEmits<{
  react: [emoji: string];
  togglePicker: [];
  closePicker: [];
  reply: [];
  replyInThread: [];
  togglePin: [];
  edit: [];
  retract: [];
}>();

const quickEmojis = QUICK_REACTION_EMOJIS;
const pickerButtonEl = ref<HTMLButtonElement | null>(null);

// Hidden at rest; `visibilityClass` (from useMessageActionSurfaces)
// reveals it on row hover/focus or pins it open.
const bar = cx("wd-msg__actions", css({
  position: "absolute",
  top: "-16px",
  right: "4",
  zIndex: "sticky",
  display: "flex",
  alignItems: "center",
  gap: "0.5",
  p: "0.5",
  borderRadius: "control",
  bg: "surface",
  border: "1px solid {colors.border}",
  boxShadow: "elev.2",
  opacity: 0,
  translate: "0 4px",
  pointerEvents: "none",
  transition: "opacity {durations.fast} {easings.out}, translate {durations.fast} {easings.out}",
  _motionReduce: { translate: "0 0" },
  _coarse: { display: "none" },
  // Reaction mode (the toolbar is then a live status region).
  "&[role=status]": { borderColor: "action.text", bg: "action.subtle" },
}));
const btn = cx(iconbtn({ size: "sm" }), css({ position: "relative", "&[aria-expanded=true]": { bg: "state.selected", color: "fg" } }));
const emojiBtn = cx(btn, css({ fontSize: "md", lineHeight: 1 }));
const dangerBtn = cx(btn, css({ _hover: { color: "danger.text", bg: "danger.subtle" } }));
const keycap = css({
  position: "absolute",
  top: "-6px",
  right: "-4px",
  display: "grid",
  placeItems: "center",
  width: "16px",
  height: "16px",
  borderRadius: "pill",
  border: "1px solid {colors.action.text}",
  bg: "surface",
  color: "action.text",
  fontSize: "10px",
  fontWeight: 700,
  lineHeight: 1,
  fontVariantNumeric: "tabular-nums",
});
const pickerWrap = css({ position: "relative", display: "flex" });
const divider = css({ width: "1px", height: "20px", mx: "0.5", bg: "border" });
</script>

<template>
  <!-- Floating action toolbar — desktop-only hover/focus affordance. On
       touch devices (where hover never fires) long-press opens the action
       sheet instead, so this toolbar stays hidden and we never show two
       emoji rails at once. -->
  <div
    :class="cx(bar, visibilityClass)"
    :role="reactionModeSelected ? 'status' : undefined"
    :aria-live="reactionModeSelected ? 'polite' : undefined"
  >
    <AppTooltip v-for="(e, index) in quickEmojis" :key="e" :label="`React with ${e}`">
      <button
        type="button"
        :class="emojiBtn"
        :aria-label="`React to message with ${e}`"
        @click="emit('react', e)"
      >
        <span
          v-if="reactionModeSelected"
          :class="keycap"
          aria-hidden="true"
        >{{ index + 1 }}</span>
        {{ e }}
      </button>
    </AppTooltip>
    <div :class="pickerWrap">
      <AppTooltip label="Add reaction">
        <button
          ref="pickerButtonEl"
          type="button"
          :class="btn"
          aria-label="Add reaction"
          :aria-expanded="pickerOpen"
          aria-haspopup="dialog"
          @click="emit('togglePicker')"
        >
          <SmilePlus aria-hidden="true" />
        </button>
      </AppTooltip>
      <EmojiPicker
        :open="pickerOpen"
        :anchor-el="pickerButtonEl"
        @select="(emoji) => emit('react', emoji)"
        @close="emit('closePicker')"
      />
    </div>
    <AppTooltip v-if="canReply" label="Reply">
      <button
        type="button"
        :class="btn"
        aria-label="Reply to message"
        @click="emit('reply')"
      >
        <Reply aria-hidden="true" />
      </button>
    </AppTooltip>
    <AppTooltip :label="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'">
      <button
        type="button"
        :class="btn"
        :aria-label="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
        @click="emit('replyInThread')"
      >
        <MessageSquare aria-hidden="true" />
      </button>
    </AppTooltip>
    <AppTooltip v-if="canPin" :label="isPinned ? 'Unpin from channel' : 'Pin to channel'">
      <button
        type="button"
        :class="btn"
        :aria-label="isPinned ? 'Unpin from channel' : 'Pin to channel'"
        @click="emit('togglePin')"
      >
        <component :is="isPinned ? PinOff : Pin" aria-hidden="true" />
      </button>
    </AppTooltip>
    <template v-if="isSelf">
      <div :class="divider" />
      <AppTooltip label="Edit message">
        <button
          type="button"
          :class="btn"
          aria-label="Edit message"
          @click="emit('edit')"
        >
          <Pencil aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip label="Delete message">
        <button
          type="button"
          :class="dangerBtn"
          aria-label="Delete message"
          @click="emit('retract')"
        >
          <Trash2 aria-hidden="true" />
        </button>
      </AppTooltip>
    </template>
  </div>
</template>
