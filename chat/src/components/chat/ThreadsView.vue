<script setup lang="ts">
import { computed } from "vue";
import { Menu, MessagesSquare } from "lucide-vue-next";
import { connectionStore } from "@/lib/connection-store";
import type { ChannelSummary } from "@/lib/chat-types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import ThreadsListPanel from "@/components/chat/ThreadsListPanel.vue";

const props = defineProps<{
  channels: readonly ChannelSummary[];
  onSelectThreadEntry: (channelJid: string, threadId: string) => void | Promise<void>;
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
    <ThreadsListPanel :xmpp-client="xmppClient" :channels="channels" @open-thread="openThread" />
  </div>
</template>
