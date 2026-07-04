<script setup lang="ts">
import { ref, watch } from "vue";
import { MessageSquare, Pencil, Pin, PinOff, Reply, SmilePlus, Trash2 } from "lucide-vue-next";
import EmojiPicker from "@/components/chat/EmojiPicker.vue";
import { QUICK_REACTION_EMOJIS } from "@/lib/reaction-mode";

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
      class="z-modal fixed inset-0 flex items-end sm:items-center justify-center animate-fade-in"
      role="presentation"
    >
      <div class="absolute inset-0 bg-background/60 backdrop-blur-sm" @click="emit('close')" />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Message actions"
        class="chat-action-sheet-stack relative w-full sm:max-w-sm glass-panel border border-border rounded-t-lg sm:rounded-lg shadow-2xl animate-slide-up p-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]"
        @pointerdown.stop
      >
        <div class="chat-action-sheet-handle sm:hidden">
          <div class="h-1 w-10 rounded-full bg-muted-foreground/30" />
        </div>

        <template v-if="sheetView === 'actions'">
          <div class="chat-action-sheet-reactions">
            <button
              v-for="e in quickEmojis"
              :key="`sheet-${e}`"
              type="button"
              class="type-emoji-sheet h-12 flex items-center justify-center rounded-lg hover:bg-muted active:bg-muted transition-colors"
              :aria-label="`React with ${e}`"
              @click="emit('react', e)"
            >{{ e }}</button>
            <button
              type="button"
              class="h-12 flex items-center justify-center rounded-lg text-muted-foreground hover:text-foreground hover:bg-muted active:bg-muted transition-colors"
              aria-label="More reactions"
              @click="sheetView = 'emoji'"
            >
              <SmilePlus class="w-5 h-5" aria-hidden="true" />
            </button>
          </div>

          <button
            v-if="canReply"
            type="button"
            class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
            @click="emit('reply')"
          >
            <Reply class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>Reply</span>
          </button>
          <button
            type="button"
            class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
            @click="emit('replyInThread')"
          >
            <MessageSquare class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>{{ threadReplyCount > 0 ? "Open thread" : "Reply in thread" }}</span>
          </button>
          <button
            v-if="canPin"
            type="button"
            class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
            @click="emit('togglePin')"
          >
            <component :is="isPinned ? PinOff : Pin" class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
            <span>{{ isPinned ? "Unpin from channel" : "Pin to channel" }}</span>
          </button>
          <template v-if="isSelf">
            <button
              type="button"
              class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg hover:bg-muted active:bg-muted transition-colors text-left"
              @click="emit('edit')"
            >
              <Pencil class="w-5 h-5 text-muted-foreground" aria-hidden="true" />
              <span>Edit</span>
            </button>
            <button
              type="button"
              class="type-field w-full flex items-center gap-3 px-3 h-12 rounded-lg text-destructive hover:bg-destructive/10 active:bg-destructive/10 transition-colors text-left"
              @click="emit('retract')"
            >
              <Trash2 class="w-5 h-5" aria-hidden="true" />
              <span>Delete</span>
            </button>
          </template>
          <button
            type="button"
            class="type-field sm:hidden w-full h-12 rounded-lg text-muted-foreground hover:bg-muted active:bg-muted transition-colors"
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
