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
  <div class="border-t border-foreground bg-background px-8 py-6">
    <div class="flex gap-4">
      <div class="flex-1">
        <textarea
          v-model="draft"
          :placeholder="`Message #${channelName}`"
          :disabled="disabled"
          rows="1"
          class="w-full font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground px-3 py-2.5 bg-background resize-none text-sm"
          @keydown="onKeydown"
        />
      </div>
      <button
        class="h-11 w-11 flex items-center justify-center bg-foreground text-background hover:bg-foreground/90 transition-colors disabled:opacity-50"
        :disabled="isSending || disabled || !draft.trim()"
        @click="emit('send')"
      >
        <Send class="w-4 h-4" />
      </button>
    </div>
  </div>
</template>
