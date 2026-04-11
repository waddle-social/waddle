<script setup lang="ts">
import { ref } from "vue";
import { Send, Image } from "lucide-vue-next";
import GifPicker from "@/components/chat/GifPicker.vue";

const draft = defineModel<string>("draft", { required: true });

defineProps<{
  channelName: string;
  isSending: boolean;
  disabled: boolean;
  tenorApiKey: string;
}>();

const emit = defineEmits<{
  send: [];
  typing: [];
  selectGif: [url: string];
}>();

const showGifPicker = ref(false);

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    emit("send");
  }
}

function onGifSelected(url: string) {
  showGifPicker.value = false;
  emit("selectGif", url);
}
</script>

<template>
  <div class="relative h-16 border-t border-foreground bg-background px-6 flex items-center gap-3 flex-shrink-0">
    <GifPicker
      v-if="showGifPicker"
      :api-key="tenorApiKey"
      @select="onGifSelected"
      @close="showGifPicker = false"
    />
    <button
      class="h-9 w-9 flex items-center justify-center hover:bg-muted transition-colors flex-shrink-0"
      :class="showGifPicker ? 'bg-muted' : ''"
      title="GIF"
      :disabled="disabled"
      @click="showGifPicker = !showGifPicker"
    >
      <Image class="w-3.5 h-3.5" />
    </button>
    <input
      :value="draft"
      :placeholder="`Message #${channelName}`"
      :disabled="disabled"
      class="flex-1 font-mono border border-foreground focus:outline-none focus:ring-2 focus:ring-foreground px-3 bg-background text-sm h-9"
      @input="draft = ($event.target as HTMLInputElement).value; emit('typing')"
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
