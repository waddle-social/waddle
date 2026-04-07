<script setup lang="ts">
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import { formatStamp } from "@/composables/useMessaging";

defineProps<{
  message: TimelineMessage;
}>();
</script>

<template>
  <div
    class="flex gap-4 border border-foreground p-4 transition-colors"
    :class="message.isSelf ? 'bg-muted/30' : ''"
  >
    <AppAvatar :name="message.author" size="md" />
    <div class="flex-1 min-w-0">
      <div class="flex items-baseline gap-3 mb-1">
        <span class="font-mono font-bold text-sm">{{ message.author }}</span>
        <span class="text-xs font-mono text-muted-foreground">
          {{ formatStamp(message.createdAt) }}
        </span>
      </div>
      <p class="text-sm leading-relaxed whitespace-pre-wrap break-words">{{ message.body }}</p>
    </div>
  </div>
</template>
