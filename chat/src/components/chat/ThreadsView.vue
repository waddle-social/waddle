<script setup lang="ts">
import { computed } from "vue";
import { connectionStore } from "@/lib/connection-store";
import type { ChannelSummary } from "@/lib/chat-types";
import type { CallMedia } from "@/lib/calls/types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import { resolveChannelIdForRoomJid } from "@/lib/threads-channel-resolve";
import ThreadsListPanel from "@/components/chat/ThreadsListPanel.vue";
import InboxTabs from "@/components/chat/InboxTabs.vue";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import { pageClasses as pc } from "@/components/chat/inbox-classes";

const props = defineProps<{
  channels: readonly ChannelSummary[];
  /** Resolves a thread starter's avatar JID from loaded rows; unknown → initials. */
  rootAuthorJid?: (entry: WasmThreadEntry) => string | null;
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
  <SettingsMobileHeader title="Inbox" @open-nav="emit('openNav')" />
  <div :class="pc.scroll">
    <div :class="pc.page">
      <header :class="pc.head">
        <div :class="pc.headText">
          <h1 :class="pc.title">Inbox</h1>
          <InboxTabs current="threads" />
        </div>
      </header>
      <ThreadsListPanel
        :xmpp-client="xmppClient"
        :channels="channels"
        :root-author-jid="rootAuthorJid"
        @open-thread="openThread"
        @join-call="joinCall"
      />
    </div>
  </div>
</template>
