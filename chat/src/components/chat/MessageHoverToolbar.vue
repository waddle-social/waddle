<script setup lang="ts">
import { ref } from "vue";
import { MessageSquare, Pencil, Pin, PinOff, Reply, SmilePlus, Trash2 } from "lucide-vue-next";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
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
      'chat-hover-action-toolbar absolute -top-4 right-3 flex items-center gap-1 transition-[opacity,transform] duration-150 ease-out bg-card border border-border rounded-lg shadow-[0_10px_28px_-12px_var(--glow-strong),0_4px_12px_-4px_color-mix(in_oklab,var(--foreground)_20%,transparent)] p-1 [@media(pointer:coarse)]:hidden',
      visibilityClass,
      reactionModeSelected ? 'chat-hover-action-toolbar--reaction-mode' : '',
    ]"
    :role="reactionModeSelected ? 'status' : undefined"
    :aria-live="reactionModeSelected ? 'polite' : undefined"
  >
    <AppTooltip v-for="(e, index) in quickEmojis" :key="e" :label="`React with ${e}`">
      <button
        type="button"
        class="chat-hover-action-toolbar-btn type-emoji-button relative h-8 w-8 flex items-center justify-center rounded-md hover:bg-muted motion-safe:hover:scale-110"
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
    </AppTooltip>
    <div class="relative">
      <AppTooltip label="Add reaction">
        <button
          ref="pickerButtonEl"
          type="button"
          class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
          :class="pickerOpen ? 'bg-muted text-foreground' : ''"
          aria-label="Add reaction"
          :aria-expanded="pickerOpen"
          aria-haspopup="dialog"
          @click="emit('togglePicker')"
        >
          <SmilePlus class="w-4 h-4" aria-hidden="true" />
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
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        aria-label="Reply to message"
        @click="emit('reply')"
      >
        <Reply class="w-4 h-4" aria-hidden="true" />
      </button>
    </AppTooltip>
    <AppTooltip :label="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'">
      <button
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        :aria-label="threadReplyCount > 0 ? 'Open thread' : 'Reply in thread'"
        @click="emit('replyInThread')"
      >
        <MessageSquare class="w-4 h-4" aria-hidden="true" />
      </button>
    </AppTooltip>
    <AppTooltip v-if="canPin" :label="isPinned ? 'Unpin from channel' : 'Pin to channel'">
      <button
        type="button"
        class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
        :aria-label="isPinned ? 'Unpin from channel' : 'Pin to channel'"
        @click="emit('togglePin')"
      >
        <component :is="isPinned ? PinOff : Pin" class="w-4 h-4" aria-hidden="true" />
      </button>
    </AppTooltip>
    <template v-if="isSelf">
      <div class="w-px h-5 bg-border mx-0.5" />
      <AppTooltip label="Edit message">
        <button
          type="button"
          class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-muted"
          aria-label="Edit message"
          @click="emit('edit')"
        >
          <Pencil class="w-4 h-4" aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip label="Delete message">
        <button
          type="button"
          class="chat-hover-action-toolbar-btn h-8 w-8 flex items-center justify-center rounded-md text-muted-foreground hover:text-destructive hover:bg-destructive/10"
          aria-label="Delete message"
          @click="emit('retract')"
        >
          <Trash2 class="w-4 h-4" aria-hidden="true" />
        </button>
      </AppTooltip>
    </template>
  </div>
</template>
