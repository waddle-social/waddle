<script setup lang="ts">
import { computed } from "vue";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import type { CallMedia } from "@/lib/calls/types";
import { threadDisplayTitle } from "@/lib/threads-view-filters";
import { useCallAnchorCardState, wasmThreadEntryToAnchorMessage } from "@/lib/call-thread-anchor";
import CallAnchorCard from "@/components/calls/CallAnchorCard.vue";

const props = defineProps<{
  entry: WasmThreadEntry;
  markingRead?: boolean;
}>();

const emit = defineEmits<{
  open: [entry: WasmThreadEntry];
  markRead: [entry: WasmThreadEntry];
  joinCall: [entry: WasmThreadEntry, media: CallMedia];
}>();

// A MUC call-thread row shares the one live-state composable and the one
// Join path with the in-channel anchor card and the call banner, so the
// global Threads view reflects the same live/ended call state. Rows that
// don't anchor a MUC call (DM anchors, plain threads) keep the title row.
const anchorMessage = computed(() => wasmThreadEntryToAnchorMessage(props.entry));
const callState = useCallAnchorCardState(
  () => anchorMessage.value ?? { body: "", author: "", threadId: null, callThread: undefined },
  () => props.entry.channel,
  () => props.entry.reply_count,
);
const isCallThread = computed(() => callState.value !== null);

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

const title = computed(() => threadDisplayTitle(props.entry));
</script>

<template>
  <div class="chat-thread-row glass-panel flex w-full items-stretch gap-2 rounded-md px-3 py-2 hover:bg-sidebar-accent/35">
    <div
      v-if="isCallThread && callState"
      class="call-thread-row__open min-w-0 flex-1"
      @click="emit('open', entry)"
    >
      <CallAnchorCard
        :state="callState"
        @join="callState && emit('joinCall', entry, callState.media)"
        @open-thread="emit('open', entry)"
      />
    </div>
    <button
      v-else
      type="button"
      class="min-w-0 flex-1 text-left"
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
    <button
      v-if="entry.has_unread"
      type="button"
      class="type-caption shrink-0 self-center rounded-md border border-border px-2 py-1 text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:opacity-60"
      :disabled="props.markingRead"
      @click.stop="emit('markRead', entry)"
    >
      {{ props.markingRead ? "Marking..." : "Mark read" }}
    </button>
  </div>
</template>
