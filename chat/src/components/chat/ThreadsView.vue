<script setup lang="ts">
import { computed } from "vue";
import { connectionStore } from "@/lib/connection-store";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import ThreadsListPanel from "@/components/chat/ThreadsListPanel.vue";

const xmppClient = computed(() => connectionStore.client);

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
    <ThreadsListPanel :xmpp-client="xmppClient" @open-thread="openThread" />
  </div>
</template>
