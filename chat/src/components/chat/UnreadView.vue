<script setup lang="ts">
import { computed, ref } from "vue";
import { Hash, ListTree, RefreshCw } from "lucide-vue-next";
import spotStickers from "@waddle/design/assets/spot-stickers.svg?url";
import { css } from "styled-system/css";
import { badge, btn, waddle } from "styled-system/recipes";
import { connectionStore } from "@/lib/connection-store";
import type { ChannelSummary } from "@/lib/chat-types";
import type { InboxState } from "@/services/inbox";
import { useUnreadOverview } from "@/lib/unread-overview-state";
import UnreadMessageRow from "@/components/chat/UnreadMessageRow.vue";
import InboxTabs from "@/components/chat/InboxTabs.vue";
import SettingsMobileHeader from "@/components/chat/SettingsMobileHeader.vue";
import { pageClasses as pc } from "@/components/chat/inbox-classes";

const props = defineProps<{
  channels: readonly ChannelSummary[];
  inboxState: InboxState;
  onSelectChannel: (channelId: string) => void | Promise<void>;
  onSelectThread: (channelId: string, threadId: string) => void | Promise<void>;
  onRefreshInbox?: () => void | Promise<unknown>;
}>();

const emit = defineEmits<{
  openNav: [];
}>();

const { groups, isLoading, error, refresh } = useUnreadOverview({
  xmppClient: computed(() => connectionStore.client),
  session: computed(() => connectionStore.session),
  channels: computed(() => props.channels),
  inboxState: computed(() => props.inboxState),
});

const countClass = badge();
const refreshButtonClass = btn({ size: "sm" });
const openButtonClass = btn({ size: "sm" });
const loaderClass = waddle();
const spinClass = css({ _motionSafe: { animation: "spin 1s linear infinite" } });

// App-Unread board: one card per room, its new messages, then its threads.
const cardClass = css({ borderRadius: "panel", border: "1px solid {colors.border.soft}", bg: "surface", overflow: "hidden" });
const cardHeadClass = css({
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  padding: "{spacing.2.5} {spacing.3} {spacing.2.5} {spacing.4}",
  borderBottom: "1px solid {colors.border.soft}",
  "& > svg": { width: "16px", height: "16px", flex: "none", color: "fg.soft" },
});
const cardTitleClass = css({ margin: 0, flex: 1, minWidth: 0, font: "700 15px/1.3 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const messagesClass = css({ display: "flex", flexDirection: "column" });
const threadClass = css({ borderTop: "1px solid {colors.border.soft}" });
const threadButtonClass = css({
  display: "flex",
  alignItems: "center",
  gap: "2",
  width: "100%",
  minHeight: "40px",
  padding: "{spacing.2} {spacing.4}",
  border: 0,
  bg: "transparent",
  color: "action.text",
  fontSize: "13px",
  fontWeight: 600,
  textAlign: "left",
  cursor: "pointer",
  transition: "background-color {durations.fast} {easings.out}",
  _hover: { bg: "state.hover" },
  _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "-3px" },
  "& > svg": { width: "16px", height: "16px", flex: "none" },
});
const threadTitleClass = css({ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });

const hasGroups = computed(() => groups.value.length > 0);
const isRefreshingInbox = ref(false);
const isRefreshBusy = computed(() => isLoading.value || isRefreshingInbox.value);

async function refreshUnread() {
  if (isRefreshBusy.value) return;
  isRefreshingInbox.value = true;
  try {
    await props.onRefreshInbox?.();
    await refresh();
  } finally {
    isRefreshingInbox.value = false;
  }
}
</script>

<template>
  <SettingsMobileHeader title="Inbox" @open-nav="emit('openNav')" />
  <div :class="pc.scroll">
    <div :class="pc.page">
      <header :class="pc.head">
        <div :class="pc.headText">
          <h1 :class="pc.title">Inbox</h1>
          <InboxTabs current="unread" />
        </div>
        <button
          type="button"
          :class="refreshButtonClass"
          :disabled="isRefreshBusy"
          aria-label="Refresh unread"
          @click="refreshUnread()"
        >
          <RefreshCw :class="isRefreshBusy ? spinClass : undefined" aria-hidden="true" />
          Refresh
        </button>
      </header>

      <div v-if="isLoading && !hasGroups" :class="pc.status" role="status" aria-busy="true">
        <span :class="loaderClass" aria-hidden="true"><i></i><i></i><i></i></span>
        Loading unread…
      </div>

      <div v-else-if="error && !hasGroups" :class="pc.alert" role="alert">
        Couldn't load unread: {{ error }}
      </div>

      <div v-else-if="!hasGroups" :class="pc.empty" role="status">
        <img :src="spotStickers" alt="" />
        <h2 :class="pc.emptyTitle">All caught up</h2>
        <p>Nothing unread. The room is yours.</p>
      </div>

      <template v-else>
        <section
          v-for="group in groups"
          :key="group.roomJid"
          :class="cardClass"
          :aria-label="group.channelName"
        >
          <header :class="cardHeadClass">
            <Hash aria-hidden="true" />
            <h2 :class="cardTitleClass">{{ group.channelName }}</h2>
            <span
              v-if="group.channelUnreadCount > 0"
              :class="countClass"
              :aria-label="`${group.channelUnreadCount} unread`"
            >{{ group.channelUnreadCount }}</span>
            <button
              type="button"
              :class="openButtonClass"
              :aria-label="`Open ${group.channelName}`"
              @click="props.onSelectChannel(group.channelId)"
            >Open</button>
          </header>

          <div v-if="group.channelMessages.length > 0" :class="messagesClass">
            <UnreadMessageRow
              v-for="message in group.channelMessages"
              :key="message.id"
              :message="message"
            />
          </div>

          <div
            v-for="thread in group.threads"
            :key="thread.threadId"
            :class="threadClass"
          >
            <button
              type="button"
              :class="threadButtonClass"
              @click="props.onSelectThread(group.channelId, thread.threadId)"
            >
              <ListTree aria-hidden="true" />
              <span :class="threadTitleClass">{{ thread.title }}</span>
              <span
                :class="countClass"
                :aria-label="`${thread.unreadCount} unread`"
              >{{ thread.unreadCount }}</span>
            </button>
            <div :class="messagesClass">
              <UnreadMessageRow
                v-for="message in thread.messages"
                :key="message.id"
                :message="message"
              />
            </div>
          </div>
        </section>
      </template>
    </div>
  </div>
</template>
