<script setup lang="ts">
import { computed } from "vue";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";

const props = defineProps<{
  entry: WasmThreadEntry;
}>();

const emit = defineEmits<{
  open: [entry: WasmThreadEntry];
}>();

const recencyLabel = computed(() => {
  const ts = Date.parse(props.entry.last_activity);
  if (Number.isNaN(ts)) return "";
  const deltaSec = Math.floor((Date.now() - ts) / 1000);
  if (deltaSec < 60) return "just now";
  if (deltaSec < 3600) return `${Math.floor(deltaSec / 60)}m ago`;
  if (deltaSec < 86_400) return `${Math.floor(deltaSec / 3600)}h ago`;
  return `${Math.floor(deltaSec / 86_400)}d ago`;
});

const channelLabel = computed(() => {
  const local = props.entry.channel.split("@")[0] ?? props.entry.channel;
  return `#${local}`;
});

const title = computed(
  () =>
    props.entry.thread_title ??
    props.entry.preview ??
    "Thread",
);
</script>

<template>
  <button
    type="button"
    class="chat-thread-row glass-panel w-full rounded-md px-3 py-2 text-left hover:bg-sidebar-accent/35"
    @click="emit('open', entry)"
  >
    <div class="flex items-center justify-between gap-2">
      <div class="min-w-0 flex-1">
        <div class="type-card-title truncate">{{ title }}</div>
        <div class="type-caption text-muted-foreground truncate">
          {{ channelLabel }} · {{ recencyLabel }}
          <span v-if="entry.reply_count > 0"> · {{ entry.reply_count }} replies</span>
        </div>
      </div>
      <span
        v-if="entry.has_unread"
        class="type-count-badge inline-flex min-w-[18px] h-[18px] items-center justify-center rounded-full bg-primary px-1 text-primary-foreground"
        :aria-label="`${entry.unread} unread`"
      >
        {{ entry.unread }}
      </span>
    </div>
  </button>
</template>
