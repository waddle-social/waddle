<script setup lang="ts">
// PinnedPanel — right-rail panel listing the room's pinned messages
// (#414). Hydrated by the chat-app-controller via fetchRoomPins on
// room entry; live-updated by the pin-event observer wired into the
// XmppClient. Mutually exclusive with ThreadPanel in the right rail
// — the parent (ChatReadyShell) gates rendering on
// ui.showPinnedPanel.
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Pin, X } from "lucide-vue-next";

import { $pinnedRooms } from "@/stores/pinned-messages";

const props = defineProps<{
  roomJid: string;
  channelName: string;
}>();

const emit = defineEmits<{
  close: [];
  jumpToMessage: [stanzaId: string];
}>();

const pinnedRooms = useStore($pinnedRooms);
const state = computed(() => pinnedRooms.value.get(props.roomJid) ?? null);
const entries = computed(() => state.value?.entries ?? []);
const hydrated = computed(() => state.value?.hydrated ?? false);

function relativeTime(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const seconds = Math.max(1, Math.round((Date.now() - t) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  return `${days}d ago`;
}
</script>

<template>
  <aside class="pinned-panel flex flex-col h-full bg-background border-l border-border">
    <header class="flex items-center justify-between px-4 h-14 border-b border-border">
      <div class="flex items-center gap-2 min-w-0">
        <Pin class="w-4 h-4 text-muted-foreground" aria-hidden="true" />
        <h2 class="type-heading-sm truncate">Pinned in {{ channelName }}</h2>
      </div>
      <button
        type="button"
        class="rounded p-1 hover:bg-muted"
        aria-label="Close pinned messages"
        @click="emit('close')"
      >
        <X class="w-5 h-5" />
      </button>
    </header>

    <div v-if="!hydrated" class="flex-1 flex items-center justify-center text-muted-foreground type-field">
      Loading pinned messages…
    </div>
    <div
      v-else-if="entries.length === 0"
      class="flex-1 flex flex-col items-center justify-center text-muted-foreground type-field gap-1 px-6"
    >
      <Pin class="w-6 h-6" aria-hidden="true" />
      <p>No pinned messages yet.</p>
      <p class="text-xs">Admins can pin a message from the message menu.</p>
    </div>
    <ol v-else class="flex-1 overflow-y-auto divide-y divide-border" role="list">
      <li
        v-for="entry in entries"
        :key="entry.target_stanza_id"
        class="px-4 py-3 cursor-pointer hover:bg-muted/40 focus-within:bg-muted/40"
        tabindex="0"
        @click="emit('jumpToMessage', entry.target_stanza_id)"
        @keydown.enter.prevent="emit('jumpToMessage', entry.target_stanza_id)"
        @keydown.space.prevent="emit('jumpToMessage', entry.target_stanza_id)"
      >
        <div class="flex items-baseline justify-between gap-2 mb-0.5">
          <span class="type-field font-medium truncate">
            {{ entry.preview.author_nick ?? entry.preview.author_jid }}
          </span>
          <span class="type-field-xs text-muted-foreground shrink-0">
            {{ relativeTime(entry.preview.message_timestamp) }}
          </span>
        </div>
        <p class="type-field-sm text-muted-foreground line-clamp-3 break-words">
          {{ entry.preview.text || "(no preview text)" }}
        </p>
        <p class="type-field-xs text-muted-foreground mt-1">
          Pinned by {{ entry.pinner_jid }} · {{ relativeTime(entry.pinned_at) }}
        </p>
      </li>
    </ol>
  </aside>
</template>
