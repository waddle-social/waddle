<script setup lang="ts">
import { computed } from "vue";
import { Menu, MessagesSquare } from "lucide-vue-next";
import { connectionStore } from "@/lib/connection-store";
import type { ChannelSummary } from "@/lib/chat-types";
import type { CallMedia } from "@/lib/calls/types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import { resolveChannelIdForRoomJid } from "@/lib/threads-channel-resolve";
import ThreadsListPanel from "@/components/chat/ThreadsListPanel.vue";

const props = defineProps<{
  channels: readonly ChannelSummary[];
  onSelectThread: (channelId: string, threadId: string) => void | Promise<void>;
  onJoinChannelCall: (channelId: string | null, roomJid: string, media: CallMedia) => void;
}>();

const xmppClient = computed(() => connectionStore.client);

const emit = defineEmits<{
  openNav: [];
}>();

// Clicking a row hands off to the controller's `onSelectThread`, which
// (a) selects the channel that hosts the thread — switching the URL
// from `/threads` to `/r/<channel>?thread=<rootId>` via the existing
// watchers — and (b) opens the ThreadPanel reader. The user lands
// directly inside the thread; a hard refresh of the resulting URL
// restores the same view.
async function openThread(entry: WasmThreadEntry) {
  const channelId = resolveChannelIdForRoomJid(entry.channel, props.channels);
  if (!channelId) return;
  await props.onSelectThread(channelId, entry.thread_id);
}

// Join routes through the same shared handler the call banner and the
// in-channel anchor card use (`joinChannelCallFromActivity` in the shell),
// resolving the row's channel via the same lookup as `openThread`.
function joinCall(entry: WasmThreadEntry, media: CallMedia) {
  const channelId = resolveChannelIdForRoomJid(entry.channel, props.channels);
  props.onJoinChannelCall(channelId, entry.channel, media);
}
</script>

<template>
  <div class="chat-content-pane">
    <header class="md:hidden flex items-center gap-2 border-b border-border bg-background px-[var(--chat-content-inline)] py-3">
      <button
        type="button"
        class="inline-flex h-8 w-8 items-center justify-center rounded-md border border-transparent text-muted-foreground hover:bg-muted/50 hover:text-foreground"
        aria-label="Open navigation"
        @click="emit('openNav')"
      >
        <Menu class="h-4 w-4" aria-hidden="true" />
      </button>
      <span class="flex h-9 w-9 items-center justify-center rounded-full bg-primary/10 text-primary">
        <MessagesSquare class="h-4.5 w-4.5" aria-hidden="true" />
      </span>
      <h1 class="type-pane-title text-foreground leading-tight">Threads</h1>
    </header>
    <ThreadsListPanel
      :xmpp-client="xmppClient"
      :channels="channels"
      @open-thread="openThread"
      @join-call="joinCall"
    />
  </div>
</template>
