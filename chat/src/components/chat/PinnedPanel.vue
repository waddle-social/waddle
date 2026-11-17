<script setup lang="ts">
// PinnedPanel — right-rail panel listing the room's pinned messages
// (#414). Hydrated by the chat-app-controller via fetchRoomPins on
// room entry; live-updated by the pin-event observer wired into the
// XmppClient. Mutually exclusive with ThreadPanel in the right rail
// — the parent (ChatReadyShell) gates rendering on
// ui.showPinnedPanel.
//
// Rich preview: each entry resolves to a TimelineMessage from either
// (a) the in-memory channel timeline or (b) the pinned-message body
// cache populated by the panel-open hydration service. The shared
// `<MessageBody compact />` renders images, video, audio, PDFs,
// downloadables, and extension cards. Empty preview.text + no live
// body → "Original message no longer available." italic fallback.
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Pin, X } from "lucide-vue-next";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";

import { $pinnedRooms } from "@/stores/pinned-messages";
import { $pinnedMessageBodies } from "@/stores/pinned-message-bodies";
import MessageBody from "@/components/chat/MessageBody.vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { WasmPinEntry } from "@/lib/xmpp/wasm-types";

const props = defineProps<{
  roomJid: string;
  channelName: string;
  /** Optional — when present, used to short-circuit MAM cache lookups
   * for pinned entries that already live in the loaded timeline. */
  timelineMessages?: ReadonlyArray<TimelineMessage>;
}>();

const emit = defineEmits<{
  close: [];
  jumpToMessage: [stanzaId: string];
}>();

const pinnedRooms = useStore($pinnedRooms);
const pinnedBodies = useStore($pinnedMessageBodies);
const state = computed(() => pinnedRooms.value.get(props.roomJid) ?? null);
const entries = computed(() => state.value?.entries ?? []);
const hydrated = computed(() => state.value?.hydrated ?? false);

const timelineIndex = computed(() => {
  const map = new Map<string, TimelineMessage>();
  for (const m of props.timelineMessages ?? []) {
    map.set(m.id, m);
    if (m.reactionTargetId) map.set(m.reactionTargetId, m);
    if (m.replyableId) map.set(m.replyableId, m);
    for (const wid of m.wireIds ?? []) map.set(wid, m);
  }
  return map;
});

function liveMessageFor(stanzaId: string): TimelineMessage | null {
  const live = timelineIndex.value.get(stanzaId);
  if (live) return live as TimelineMessage;
  const cached = pinnedBodies.value.get(props.roomJid)?.get(stanzaId);
  return cached ? (cached as TimelineMessage) : null;
}

interface ResolvedEntry {
  entry: WasmPinEntry;
  live: TimelineMessage | null;
}

const resolvedEntries = computed<ResolvedEntry[]>(() =>
  entries.value.map((entry) => ({
    entry,
    live: liveMessageFor(entry.target_stanza_id),
  })),
);

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

// App-Room "Pinned" panel: a titled rail with one floe card per pin.
const cls = {
  root: css({ display: "flex", flexDirection: "column", height: "100%", minHeight: 0, bg: "bg.rail", borderLeft: "1px solid {colors.border.soft}" }),
  head: css({ display: "flex", alignItems: "center", gap: "2", flex: "none", minHeight: "header", px: "4", borderBottom: "1px solid {colors.border.soft}" }),
  title: css({ margin: 0, flex: 1, minWidth: 0, font: "700 16px/1.3 {fonts.display}", color: "fg", truncate: true }),
  state: css({ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: "1.5", px: "6", textAlign: "center", fontSize: "sm", color: "fg.muted", "& p": { margin: 0 } }),
  stateIcon: css({ width: "24px", height: "24px", color: "fg.soft" }),
  hint: css({ textStyle: "caption" }),
  list: css({ flex: 1, minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column", gap: "2", p: "3", m: 0, listStyle: "none" }),
  card: css({
    display: "flex",
    flexDirection: "column",
    gap: "1.5",
    p: "4",
    borderRadius: "panel",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
    cursor: "pointer",
    outline: "none",
    transition: "border-color {durations.fast} {easings.out}",
    _hover: { borderColor: "border.strong" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
  }),
  byline: css({ textStyle: "caption", color: "gold.text" }),
  meta: css({ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: "2" }),
  author: css({ fontSize: "sm", fontWeight: 700, color: "fg", truncate: true }),
  time: css({ textStyle: "caption", color: "fg.muted", flex: "none", fontVariantNumeric: "tabular-nums" }),
  text: css({ margin: 0, fontSize: "sm", color: "fg", lineClamp: 3, overflowWrap: "anywhere" }),
  gone: css({ margin: 0, fontSize: "sm", fontStyle: "italic", color: "fg.muted" }),
};
</script>

<template>
  <aside :class="cls.root" aria-label="Pinned messages">
    <header :class="cls.head">
      <h2 :class="cls.title">Pinned in {{ channelName }}</h2>
      <button
        type="button"
        :class="iconbtn()"
        aria-label="Close pinned messages"
        @click="emit('close')"
      >
        <X aria-hidden="true" />
      </button>
    </header>

    <div v-if="!hydrated" :class="cls.state" role="status">
      <span>Loading pinned messages…</span>
    </div>
    <div v-else-if="entries.length === 0" :class="cls.state">
      <Pin :class="cls.stateIcon" aria-hidden="true" />
      <p>No pinned messages yet.</p>
      <p :class="cls.hint">Pin a message from the message menu.</p>
    </div>
    <ol v-else :class="cls.list" role="list">
      <li
        v-for="{ entry, live } in resolvedEntries"
        :key="entry.target_stanza_id"
        :class="cls.card"
        tabindex="0"
        @click="emit('jumpToMessage', entry.target_stanza_id)"
        @keydown.enter.prevent="emit('jumpToMessage', entry.target_stanza_id)"
        @keydown.space.prevent="emit('jumpToMessage', entry.target_stanza_id)"
      >
        <span :class="cls.byline">
          Pinned by {{ entry.pinner_jid }} · {{ relativeTime(entry.pinned_at) }}
        </span>
        <div :class="cls.meta">
          <span :class="cls.author">{{ entry.preview.author_nick ?? entry.preview.author_jid }}</span>
          <span :class="cls.time">{{ relativeTime(entry.preview.message_timestamp) }}</span>
        </div>

        <!-- Rich render or fallback -->
        <template v-if="live !== null">
          <p v-if="live.isRetracted" :class="cls.gone">Message retracted</p>
          <MessageBody v-else :message="live" compact />
        </template>
        <template v-else>
          <p v-if="entry.preview.text" :class="cls.text">{{ entry.preview.text }}</p>
          <p v-else :class="cls.gone">Original message no longer available.</p>
        </template>
      </li>
    </ol>
  </aside>
</template>
