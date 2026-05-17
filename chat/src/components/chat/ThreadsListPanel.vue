<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WasmThreadEntry, WasmThreadsPage } from "@/lib/xmpp/wasm-types";
import ThreadsListRow from "@/components/chat/ThreadsListRow.vue";

const props = defineProps<{
  xmppClient: BrowserXmppClient | null;
}>();

const emit = defineEmits<{
  openThread: [entry: WasmThreadEntry];
}>();

const loading = ref(true);
const page = ref<WasmThreadsPage | null>(null);
const error = ref<string | null>(null);

const unread = computed(
  () => page.value?.entries.filter((entry) => entry.has_unread) ?? [],
);
const following = computed(
  () => page.value?.entries.filter((entry) => !entry.has_unread) ?? [],
);

onMounted(async () => {
  if (!props.xmppClient) {
    loading.value = false;
    return;
  }
  try {
    page.value = await props.xmppClient.fetchThreads({ pageSize: 50 });
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err);
  } finally {
    loading.value = false;
  }
});
</script>

<template>
  <div class="chat-panel-stack p-4">
    <h2 class="type-pane-title">Threads</h2>

    <div v-if="loading" class="type-caption text-muted-foreground" aria-busy="true">
      Loading threads…
    </div>

    <div v-else-if="error" class="type-caption text-destructive">
      Couldn't load threads: {{ error }}
    </div>

    <template v-else-if="page">
      <section v-if="unread.length > 0" class="chat-panel-stack">
        <div class="type-section-label text-muted-foreground/75">
          Unread · {{ unread.length }}
        </div>
        <ThreadsListRow
          v-for="entry in unread"
          :key="`${entry.channel}|${entry.thread_id}`"
          :entry="entry"
          @open="emit('openThread', $event)"
        />
      </section>

      <section v-if="following.length > 0" class="chat-panel-stack">
        <div class="type-section-label text-muted-foreground/75">
          Following · {{ following.length }}
        </div>
        <ThreadsListRow
          v-for="entry in following"
          :key="`${entry.channel}|${entry.thread_id}`"
          :entry="entry"
          @open="emit('openThread', $event)"
        />
      </section>

      <div
        v-if="unread.length === 0 && following.length === 0"
        class="type-caption text-muted-foreground"
      >
        No threads yet. Threads you reply to or get mentioned in will show up here.
      </div>
    </template>
  </div>
</template>
