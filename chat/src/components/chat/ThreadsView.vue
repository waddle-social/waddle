<script setup lang="ts">
import { computed } from "vue";
import { Menu } from "lucide-vue-next";
import { connectionStore } from "@/lib/connection-store";
import type { ChannelSummary } from "@/lib/chat-types";
import type { CallMedia } from "@/lib/calls/types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import { resolveChannelIdForRoomJid } from "@/lib/threads-channel-resolve";
import ThreadsListPanel from "@/components/chat/ThreadsListPanel.vue";

const props = defineProps<{
  channels: readonly ChannelSummary[];
  onSelectThreadEntry: (channelJid: string, threadId: string) => void | Promise<void>;
  onJoinChannelCall: (channelId: string | null, roomJid: string, media: CallMedia) => void;
}>();

const xmppClient = computed(() => connectionStore.client);

const emit = defineEmits<{
  openNav: [];
}>();

// Clicking a row hands off the entry's bare JID to the controller's
// `onSelectThreadEntry`, which (a) routes to the hosting surface — a
// channel selection for MUC rooms, or a DM open for partner JIDs — and
// (b) opens the ThreadPanel reader. The user lands directly inside the
// thread; a hard refresh of the resulting URL restores the same view.
async function openThread(entry: WasmThreadEntry) {
  await props.onSelectThreadEntry(entry.channel, entry.thread_id);
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
  <div class="chat-content-pane chat-pane-scroll bg-background">
    <div class="mx-auto grid w-full max-w-3xl gap-5 px-[var(--chat-content-inline)] py-6">
      <header class="flex items-center gap-3">
        <button
          type="button"
          class="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-border bg-card text-muted-foreground hover:bg-muted hover:text-foreground md:hidden"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Menu class="h-4 w-4" aria-hidden="true" />
        </button>
        <h1 class="font-display text-[30px] font-bold leading-none tracking-[-0.03em] text-foreground">Discussions</h1>
      </header>
      <ThreadsListPanel
        :xmpp-client="xmppClient"
        :channels="channels"
        @open-thread="openThread"
        @join-call="joinCall"
      />
    </div>
  </div>
</template>
