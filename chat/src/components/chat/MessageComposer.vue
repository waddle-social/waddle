<script setup lang="ts">
import { Send } from "lucide-vue-next";

const draft = defineModel<string>("draft", { required: true });

defineProps<{
  channelName: string;
  isSending: boolean;
  disabled: boolean;
}>();

const emit = defineEmits<{
  send: [];
}>();

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    emit("send");
  }
}
</script>

<template>
  <div class="h-14 border-t border-foreground bg-background px-6 flex items-center gap-3 flex-shrink-0">
    <input
      :value="draft"
      :placeholder="`Message #${channelName}`"
      :disabled="disabled"
      class="flex-1 font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground px-3 py-1.5 bg-background text-sm h-9"
      @input="draft = ($event.target as HTMLInputElement).value"
      @keydown="onKeydown"
    />
    <button
      class="h-9 w-9 flex items-center justify-center bg-foreground text-background hover:bg-foreground/90 transition-colors disabled:opacity-50 flex-shrink-0"
      :disabled="isSending || disabled || !draft.trim()"
      @click="emit('send')"
    >
      <Send class="w-3.5 h-3.5" />
    </button>
  </div>
</template>
