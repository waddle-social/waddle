<script setup lang="ts">
import { computed } from "vue";
import { Menu, MessagesSquare } from "lucide-vue-next";
import { connectionStore } from "@/lib/connection-store";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import ThreadsListPanel from "@/components/chat/ThreadsListPanel.vue";

const xmppClient = computed(() => connectionStore.client);

const emit = defineEmits<{
  openNav: [];
}>();

function openThread(entry: WasmThreadEntry) {
  // Navigate to the channel that hosts the thread. The channel page
  // currently picks the active thread off the URL hash; we land on
  // the room and the user re-opens the thread by clicking the reply
  // chip until the live thread-delta hook is in place.
  const channel = entry.channel.split("@")[0] ?? entry.channel;
  if (!channel) return;
  const target = `/r/${channel}#thread=${encodeURIComponent(entry.thread_id)}`;
  window.location.assign(target);
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
    <ThreadsListPanel :xmpp-client="xmppClient" @open-thread="openThread" />
  </div>
</template>
