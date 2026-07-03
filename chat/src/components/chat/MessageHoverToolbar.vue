<script setup lang="ts">
import { ref } from "vue";
import { MessageSquare, Pencil, Pin, PinOff, Reply, SmilePlus, Trash2 } from "lucide-vue-next";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";

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
</script>

<template>
  <!-- Floating action toolbar — desktop-only hover/focus affordance. On
       touch devices (where hover never fires) long-press opens the action
       sheet instead, so this toolbar stays hidden and we never show two
       emoji rails at once. -->
  <div
    :class="[
      'chat-hover-action-toolbar absolute -top-4 right-3 flex items-center gap-1 transition-[opacity,transform] duration-150 ease-out bg-card/95 backdrop-blur border border-border rounded-lg shadow-[0_10px_28px_-12px_var(--glow-strong),0_4px_12px_-4px_color-mix(in_oklab,var(--foreground)_20%,transparent)] p-1 [@media(pointer:coarse)]:hidden',
      visibilityClass,
      reactionModeSelected ? 'chat-hover-action-toolbar--reaction-mode' : '',
    ]"
    :role="reactionModeSelected ? 'status' : undefined"
    :aria-live="reactionModeSelected ? 'polite' : undefined"
  >
    <button
      v-for="(e, index) in quickEmojis"
      :key="e"
      type="button"
      class="chat-hover-action-toolbar-btn type-emoji-button relative h-8 w-8 flex items-center justify-center rounded-md hover:bg-muted motion-safe:hover:scale-110"
      :title="`React with ${e}`"
      :aria-label="`React to message with ${e}`"
      @click="emit('react', e)"
    >
      <span
        v-if="reactionModeSelected"
        class="chat-reaction-mode-keycap type-meta type-numeric"
        aria-hidden="true"
      >{{ index + 1 }}</span>
      {{ e }}
    </button>
    <div class="relative">
      <button
        ref="pickerButtonEl"
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        :class="pickerOpen ? 'bg-muted text-foreground' : ''"
        title="Add reaction"
        aria-label="Add reaction"
        :aria-expanded="pickerOpen"
        aria-haspopup="dialog"
        @click="emit('togglePicker')"
      >
        <SmilePlus class="w-4 h-4" aria-hidden="true" />
      </button>
      <EmojiPicker
        :open="pickerOpen"
        :anchor-el="pickerButtonEl"
        @select="(emoji) => emit('react', emoji)"
        @close="emit('closePicker')"
      />
    </div>
    <button
      v-if="canReply"
      type="button"
      class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
      title="Reply"
      aria-label="Reply to message"
      @click="emit('reply')"
    >
      <Reply class="w-4 h-4" aria-hidden="true" />
    </button>
    <button
      type="button"
      class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
      :title="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
      :aria-label="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
      @click="emit('replyInThread')"
    >
      <MessageSquare class="w-4 h-4" aria-hidden="true" />
    </button>
    <button
      v-if="canPin"
      type="button"
      class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
      :title="isPinned ? 'Unpin from channel' : 'Pin to channel'"
      :aria-label="isPinned ? 'Unpin from channel' : 'Pin to channel'"
      @click="emit('togglePin')"
    >
      <component :is="isPinned ? PinOff : Pin" class="w-4 h-4" aria-hidden="true" />
    </button>
    <template v-if="isSelf">
      <div class="w-px h-5 bg-border mx-0.5" />
      <button
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        title="Edit message"
        aria-label="Edit message"
        @click="emit('edit')"
      >
        <Pencil class="w-4 h-4" aria-hidden="true" />
      </button>
      <button
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10"
        title="Delete message"
        aria-label="Delete message"
        @click="emit('retract')"
      >
        <Trash2 class="w-4 h-4" aria-hidden="true" />
      </button>
    </template>
  </div>
</template>
