<script setup lang="ts">
import { computed } from "vue";
import { Phone, Video } from "lucide-vue-next";
import { button, count, tag } from "styled-system/recipes";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import { jidLocalpart } from "@/lib/xmpp/jid";
import type { CallMedia } from "@/lib/calls/types";
import { threadDisplayTitle } from "@/lib/threads-view-filters";
import { useCallAnchorCardState, wasmThreadEntryToAnchorMessage } from "@/lib/call-thread-anchor";
import AppAvatar from "@/components/ui/AppAvatar.vue";
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

const channelTagClass = tag({ tone: "neutral" });
const unreadCountClass = count();
const markReadClass = button({ variant: "quiet", size: "sm" });

// A MUC call-thread row shares the one live-state composable and the one
// Join path with the in-channel anchor card and the call banner, so the
// global Discussions view reflects the same live/ended call state. Rows
// that don't anchor a MUC call (DM anchors, plain threads) keep the card.
const anchorMessage = computed(() => wasmThreadEntryToAnchorMessage(props.entry));
const callState = useCallAnchorCardState(
  () => anchorMessage.value ?? { body: "", author: "", threadId: undefined, callThread: undefined },
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

const channelLabel = computed(() => `#${jidLocalpart(props.entry.channel)}`);

const title = computed(() => threadDisplayTitle(props.entry));

const replyLabel = computed(() => {
  const replies = props.entry.reply_count;
  return `${replies} ${replies === 1 ? "reply" : "replies"}`;
});

// The threads query carries only the person who started the discussion,
// so the avatar stack shows exactly that: no invented participants.
const rootAuthor = computed(() => props.entry.root_author?.trim() ?? "");

const isDmCallThread = computed(() => props.entry.callThread?.kind === "dm");
const dmCallFlagLabel = computed(() => {
  if (!isDmCallThread.value) return "";
  const hasVideo = props.entry.callThread?.media.includes("video") ?? false;
  return hasVideo ? "Video call thread" : "Call thread";
});
const DmCallFlagIcon = computed(() => {
  const hasVideo = props.entry.callThread?.media.includes("video") ?? false;
  return hasVideo ? Video : Phone;
});
</script>

<template>
  <article
    class="flex w-full items-stretch gap-3 rounded-xl border bg-card p-4 transition-colors hover:bg-muted"
    :class="entry.has_unread ? 'border-live/60' : 'border-border'"
  >
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
      class="flex min-w-0 flex-1 items-start gap-3 text-left"
      @click="emit('open', entry)"
    >
      <span v-if="rootAuthor" class="mt-0.5 flex shrink-0 items-center" aria-hidden="true">
        <AppAvatar :name="rootAuthor" size="sm" />
      </span>
      <span class="min-w-0 flex-1">
        <span class="flex min-w-0 items-center gap-2">
          <span
            v-if="isDmCallThread"
            class="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-primary/40 text-primary"
            role="img"
            :aria-label="dmCallFlagLabel"
          >
            <component :is="DmCallFlagIcon" class="h-3 w-3" aria-hidden="true" />
          </span>
          <span class="min-w-0 flex-1 truncate text-[15px] font-semibold leading-snug text-foreground">{{ title }}</span>
          <span
            v-if="entry.has_unread"
            :class="unreadCountClass"
            :aria-label="`${entry.unread} unread`"
          >{{ entry.unread }}</span>
        </span>
        <span class="mt-1 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <span :class="channelTagClass">{{ channelLabel }}</span>
          <span class="type-caption min-w-0 truncate text-muted-foreground">
            <template v-if="rootAuthor">{{ rootAuthor }} · </template>{{ replyLabel }}<template v-if="recencyLabel"> · last active {{ recencyLabel }}</template>
          </span>
        </span>
      </span>
    </button>
    <button
      v-if="entry.has_unread"
      type="button"
      :class="[markReadClass, 'shrink-0 self-center']"
      :disabled="props.markingRead"
      @click.stop="emit('markRead', entry)"
    >
      {{ props.markingRead ? "Marking..." : "Mark read" }}
    </button>
  </article>
</template>
