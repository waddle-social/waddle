<script setup lang="ts">
import { ref, watch } from "vue";
import { MessageSquare, Pencil, Pin, PinOff, Reply, SmilePlus, Trash2 } from "lucide-vue-next";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";
import { css } from "styled-system/css";
import { scrim } from "styled-system/recipes";

const props = defineProps<{
  open: boolean;
  canReply: boolean;
  threadReplyCount: number;
  canPin: boolean;
  isPinned: boolean;
  isSelf: boolean;
}>();

const emit = defineEmits<{
  react: [emoji: string];
  reply: [];
  replyInThread: [];
  togglePin: [];
  edit: [];
  retract: [];
  close: [];
}>();

const quickEmojis = QUICK_REACTION_EMOJIS;

const c = {
  layer: css({ position: "fixed", inset: 0, zIndex: "dialog", display: "flex", alignItems: "flex-end", justifyContent: "center", sm: { alignItems: "center" } }),
  scrim: scrim(),
  sheet: css({
    "--ring-bg": "{colors.surface}",
    position: "relative",
    zIndex: "dialog",
    display: "flex",
    flexDirection: "column",
    gap: "1",
    width: "100%",
    p: "3",
    pb: "max({spacing.3}, env(safe-area-inset-bottom))",
    bg: "surface",
    color: "fg",
    border: "1px solid {colors.border}",
    borderRadius: "{radii.sheet} {radii.sheet} 0 0",
    boxShadow: "overlay",
    animation: "wd-rise {durations.slow} {easings.spring.snappy} backwards",
    sm: { maxWidth: "384px", borderRadius: "sheet" },
  }),
  handle: css({ display: "flex", justifyContent: "center", pb: "1", sm: { display: "none" }, "& > div": { width: "40px", height: "4px", borderRadius: "pill", bg: "border.strong" } }),
  reactions: css({ display: "grid", gridTemplateColumns: "repeat(6, minmax(0, 1fr))", gap: "1", pb: "1" }),
  reaction: css({
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    height: "48px",
    border: 0,
    borderRadius: "control",
    bg: "transparent",
    color: "fg.muted",
    fontSize: "2xl",
    lineHeight: 1,
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}",
    _hover: { bg: "state.hover", color: "fg" },
    _active: { bg: "state.press" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "-3px" },
    "& svg": { width: "20px", height: "20px" },
  }),
  row: css({
    display: "flex",
    alignItems: "center",
    gap: "3",
    width: "100%",
    height: "48px",
    px: "3",
    border: 0,
    borderRadius: "control",
    bg: "transparent",
    color: "fg",
    font: "500 15px/1.25 {fonts.body}",
    textAlign: "left",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}",
    _hover: { bg: "state.hover" },
    _active: { bg: "state.press" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "-3px" },
    "& svg": { width: "20px", height: "20px", flex: "none", color: "fg.muted" },
    "&[data-tone=danger], &[data-tone=danger] svg": { color: "danger.text" },
    "&[data-tone=danger]:hover": { bg: "danger.subtle" },
    "&[data-cancel]": { justifyContent: "center", color: "fg.muted", sm: { display: "none" } },
  }),
};

type SheetView = "actions" | "emoji";
const sheetView = ref<SheetView>("actions");

// Every open starts on the action list; the emoji grid is only ever an
// explicit in-sheet navigation away.
watch(
  () => props.open,
  (open) => {
    if (open) sheetView.value = "actions";
  },
);
</script>

<template>
  <!-- Unified action sheet: opened by touch long-press or the MoreHorizontal
       trigger. Teleported so it escapes overflow-hidden
       ancestors; anchored at the bottom on mobile for large touch targets
       and centred when opened from a wider touch viewport. -->
  <Teleport to="body">
    <div
      v-if="open"
      :class="c.layer"
      role="presentation"
    >
      <div :class="c.scrim" @click="emit('close')" />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Message actions"
        :class="c.sheet"
        @pointerdown.stop
      >
        <div :class="c.handle">
          <div />
        </div>

        <template v-if="sheetView === 'actions'">
          <div :class="c.reactions">
            <button
              v-for="e in quickEmojis"
              :key="`sheet-${e}`"
              type="button"
              :class="c.reaction"
              :aria-label="`React with ${e}`"
              @click="emit('react', e)"
            >{{ e }}</button>
            <button
              type="button"
              :class="c.reaction"
              aria-label="More reactions"
              @click="sheetView = 'emoji'"
            >
              <SmilePlus aria-hidden="true" />
            </button>
          </div>

          <button
            v-if="canReply"
            type="button"
            :class="c.row"
            @click="emit('reply')"
          >
            <Reply aria-hidden="true" />
            <span>Reply</span>
          </button>
          <button
            type="button"
            :class="c.row"
            @click="emit('replyInThread')"
          >
            <MessageSquare aria-hidden="true" />
            <span>{{ threadReplyCount > 0 ? "Open thread" : "Reply in thread" }}</span>
          </button>
          <button
            v-if="canPin"
            type="button"
            :class="c.row"
            @click="emit('togglePin')"
          >
            <component :is="isPinned ? PinOff : Pin" aria-hidden="true" />
            <span>{{ isPinned ? "Unpin from channel" : "Pin to channel" }}</span>
          </button>
          <template v-if="isSelf">
            <button
              type="button"
              :class="c.row"
              @click="emit('edit')"
            >
              <Pencil aria-hidden="true" />
              <span>Edit</span>
            </button>
            <button
              type="button"
              :class="c.row"
              data-tone="danger"
              @click="emit('retract')"
            >
              <Trash2 aria-hidden="true" />
              <span>Delete</span>
            </button>
          </template>
          <button
            type="button"
            :class="c.row"
            data-cancel
            @click="emit('close')"
          >Cancel</button>
        </template>

        <template v-else>
          <EmojiPicker
            :open="true"
            variant="sheet"
            @select="(emoji) => emit('react', emoji)"
            @close="sheetView = 'actions'"
          />
        </template>
      </div>
    </div>
  </Teleport>
</template>
