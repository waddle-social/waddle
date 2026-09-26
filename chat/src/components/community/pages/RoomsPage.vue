<script setup lang="ts">
import { computed } from "vue";
import { Menu } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import TopicsPanel from "@/components/chat/TopicsPanel.vue";
import DmPanel from "@/components/chat/DmPanel.vue";
import { buildHomeChannelUnreadMap } from "@/home/dashboard-props";
import {
  callParticipantCountForChannel,
  callRoomJidForChannel,
} from "@/lib/calls/muc-call-indicators";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import type { CallMedia } from "@/lib/calls/types";
import type { ChannelSummary } from "@/lib/chat-types";
import type { MessageThreadEntry } from "@/channels/threads";
import { barePeerJid } from "@/lib/xmpp/jid";
import { isEventUpcomingOrOngoing } from "@/lib/xmpp-client";
import type { ChatAppController } from "@/shell/chat-app-controller";

const props = defineProps<{
  controller: ChatAppController;
  callParticipantCounts?: Record<string, number>;
  callParticipants?: Record<string, string[]>;
  callMediaByRoom?: Record<string, CallMedia>;
  managedMucDomain?: string | null;
  selfFullJid?: string | null;
  dmThreadEntries?: MessageThreadEntry[];
  joinChannelCall?: (channelId: string | null, roomJid: string, media: CallMedia) => void;
  leaveChannelCall?: (roomJid: string) => void;
  answerDm?: (peerJid: string, remoteFullJid: string, sid: string, media: CallMedia) => void;
  reconnectDm?: (peerJid: string, media: CallMedia) => void;
  endDm?: (peerJid: string, sid?: string) => void;
}>();

const emit = defineEmits<{
  openNav: [];
}>();

const {
  ui,
  waddles,
  messaging,
  dmConversations,
  channelUnread,
  stories,
  communityEvents,
  displayedMemberCount,
  displayedMemberState,
  computedChannelUnreadMap,
  groupDmConversations,
  activeChannelRoomJid,
  avatarUrlByAuthor,
  selectChannel,
  selectChannelByRoomJid,
  selectGroupDm,
  selectDm,
  onSelectThread,
  openThread,
  openCommunitySurface,
  openThreads,
  openUnread,
  openCreateChannelDialog,
  handleNewGroupDm,
  handleAddPeopleToDm,
} = props.controller;

interface RoomTile {
  channel: ChannelSummary;
  roomJid: string;
  callCount: number;
  media: CallMedia | undefined;
  nicks: readonly string[];
  unread: number;
  mentions: number;
  preview: string;
  lastUpdated: number;
  hereCount: number | null;
  hasActivity: boolean;
  kicker: string;
  live: boolean;
  isActive: boolean;
}

function roomKey(jid: string | undefined | null): string {
  return jid ? barePeerJid(jid).toLowerCase() : "";
}

const tiles = computed<RoomTile[]>(() => {
  const channels = waddles.sortedChannels.value.filter((channel) => !channel.isGroupDm);
  const unreadMap = buildHomeChannelUnreadMap(
    channels,
    computedChannelUnreadMap.value,
    messaging.mentionedChannelCounts.value,
  );
  const activeJids = messaging.activeChannels.value;
  const activeKeys = new Set([...activeJids].map(roomKey));
  const activeRoomKey = roomKey(activeChannelRoomJid.value);
  const hereCountForActiveRoom = Object.values(messaging.roomPresence.value)
    .filter((presence) => presence !== "offline").length;

  return channels
    .map((channel) => {
      const callCount = callParticipantCountForChannel(
        channel, props.callParticipantCounts, activeJids, props.managedMucDomain,
      );
      const callRoomJid = callCount > 0
        ? callRoomJidForChannel(channel, props.callParticipantCounts, activeJids, props.managedMucDomain)
        : "";
      const roomJid = callRoomJid || (channel.jid ? normalizeMucCallRoomJid(channel.jid) : "");
      const media = roomJid ? props.callMediaByRoom?.[roomJid] : undefined;
      const nicks = roomJid ? props.callParticipants?.[roomJid] ?? [] : [];
      const activity = unreadMap[channel.id] ?? { unread: 0, mentions: 0 };
      const isActive = !!activeRoomKey && roomKey(channel.jid) === activeRoomKey;
      const hereCount = isActive ? hereCountForActiveRoom : null;
      const hasActivity = !!channel.jid && activeKeys.has(roomKey(channel.jid));
      const live = callCount > 0;
      let kicker: string;
      if (live) {
        kicker = `${media?.video ? "Video" : "Voice"} huddle · ${callCount} in`;
      } else if (hereCount !== null) {
        kicker = `Room · ${hereCount} here`;
      } else if (activity.mentions > 0) {
        kicker = `Room · ${activity.mentions} mention${activity.mentions === 1 ? "" : "s"}`;
      } else if (activity.unread > 0) {
        kicker = `Room · ${activity.unread} unread`;
      } else if (hasActivity) {
        kicker = "Room · active";
      } else {
        kicker = "Room · quiet";
      }
      return {
        channel,
        roomJid,
        callCount,
        media,
        nicks,
        unread: activity.unread,
        mentions: activity.mentions,
        preview: activity.preview?.trim() ?? "",
        lastUpdated: activity.lastUpdated ?? 0,
        hereCount,
        hasActivity,
        kicker,
        live,
        isActive,
      };
    })
    .sort((a, b) =>
      b.callCount - a.callCount
      || b.mentions - a.mentions
      || b.unread - a.unread
      || b.lastUpdated - a.lastUpdated
      || a.channel.name.localeCompare(b.channel.name, undefined, { sensitivity: "base" }),
    );
});

const liveCount = computed(() => tiles.value.filter((tile) => tile.live).length);

function tileAvatars(tile: RoomTile): { nick: string; src: string | null }[] {
  return tile.nicks.slice(0, 4).map((nick) => ({
    nick,
    src: tile.isActive ? avatarUrlByAuthor.value[nick] ?? null : null,
  }));
}

function selectChannelFromPage(id: string | null, roomJid?: string) {
  ui.activeCommunitySurface.value = null;
  if (id) {
    void selectChannel(id, roomJid ? { roomJid } : undefined);
    return;
  }
  if (roomJid) void selectChannelByRoomJid(roomJid);
}

function enterRoom(tile: RoomTile) {
  selectChannelFromPage(tile.channel.id, tile.channel.jid);
}

function joinChannelCallFromPage(channelId: string | null, roomJid: string, media: CallMedia) {
  props.joinChannelCall?.(channelId, roomJid, media);
}

function joinHuddle(tile: RoomTile) {
  if (!tile.roomJid) return;
  joinChannelCallFromPage(tile.channel.id, tile.roomJid, tile.media ?? { audio: true, video: false });
}

function leaveChannelCallFromPage(roomJid: string) {
  props.leaveChannelCall?.(roomJid);
}

function answerDmFromPage(peerJid: string, remoteFullJid: string, sid: string, media: CallMedia) {
  props.answerDm?.(peerJid, remoteFullJid, sid, media);
}

function reconnectDmFromPage(peerJid: string, media: CallMedia) {
  props.reconnectDm?.(peerJid, media);
}

function endDmFromPage(peerJid: string, sid?: string) {
  props.endDm?.(peerJid, sid);
}

function selectCommunitySurface(surface: "feed" | "events") {
  openCommunitySurface(surface);
}
</script>

<template>
  <div class="community-page">
    <header class="community-page__header">
      <div class="flex min-w-0 items-start gap-3">
        <button
          type="button"
          class="community-page__mobile-nav lg:hidden"
          aria-label="Open navigation"
          @click="emit('openNav')"
        >
          <Menu class="h-4 w-4" aria-hidden="true" />
        </button>
        <div class="community-page__heading">
          <span class="community-kicker" :class="liveCount > 0 ? 'community-kicker--live' : ''">
            <span v-if="liveCount > 0" class="community-ember" aria-hidden="true" />
            {{ tiles.length }} room{{ tiles.length === 1 ? "" : "s" }}<template v-if="liveCount > 0"> · {{ liveCount }} live</template>
          </span>
          <h1 class="community-page__title">Rooms</h1>
          <p class="community-page__lead">Sorted by what is happening: huddles first, then mentions and unread.</p>
        </div>
      </div>
      <button
        v-if="waddles.canManageChannels.value"
        type="button"
        class="community-pill"
        @click="openCreateChannelDialog()"
      >
        New room
      </button>
    </header>

    <div class="community-page__body">
      <section class="community-section" aria-label="Rooms">
        <div v-if="waddles.isLoadingStructure.value && tiles.length === 0" class="community-empty">
          Finding your rooms.
        </div>
        <div v-else-if="tiles.length === 0" class="community-empty">
          No rooms yet. Create the first one and invite people in.
        </div>
        <div v-else class="community-grid">
          <article
            v-for="tile in tiles"
            :key="tile.channel.id"
            class="room-card"
            :class="tile.live ? 'room-card--live' : ''"
          >
            <span class="community-kicker" :class="tile.live ? 'community-kicker--live' : ''">
              <span v-if="tile.live" class="community-ember" aria-hidden="true" />
              {{ tile.kicker }}
            </span>
            <button
              type="button"
              class="room-card__enter"
              :aria-label="`Enter ${tile.channel.name}`"
              @click="enterRoom(tile)"
            >
              <span class="room-card__title">{{ tile.channel.name }}</span>
            </button>
            <p v-if="tile.preview" class="room-card__preview">{{ tile.preview }}</p>
            <p v-else-if="!tile.live && tile.unread === 0" class="room-card__quiet">
              It is quiet in here. Be the one who breaks the silence.
            </p>
            <div class="room-card__footer">
              <div class="flex min-w-0 items-center gap-2">
                <div v-if="tile.nicks.length > 0" class="room-card__avatars" :aria-label="`${tile.nicks.length} in the huddle`">
                  <span v-for="avatar in tileAvatars(tile)" :key="avatar.nick" class="inline-flex rounded-full ring-2 ring-card">
                    <AppAvatar :name="avatar.nick" :src="avatar.src" size="xs" />
                  </span>
                </div>
                <span v-if="tile.mentions > 0" class="room-card__badge" :aria-label="`${tile.mentions} mentions`">@{{ tile.mentions }}</span>
                <span v-else-if="tile.unread > 0" class="room-card__badge room-card__badge--unread" :aria-label="`${tile.unread} unread`">{{ tile.unread }}</span>
              </div>
              <button
                v-if="tile.live"
                type="button"
                class="community-pill community-pill--live"
                :aria-label="`Join the huddle in ${tile.channel.name}`"
                @click="joinHuddle(tile)"
              >
                Join
              </button>
              <button
                v-else
                type="button"
                class="community-pill"
                :aria-label="`Enter ${tile.channel.name}`"
                @click="enterRoom(tile)"
              >
                Enter
              </button>
            </div>
          </article>
        </div>
      </section>

      <section class="community-section" aria-label="Browse all rooms">
        <h2 class="community-section__title">Browse all rooms</h2>
        <div class="community-embed">
          <TopicsPanel
            :waddle="waddles.currentSpace.value"
            :spaces="waddles.sortedSpaces.value"
            :channels="waddles.sortedChannels.value"
            :active-channel-id="waddles.activeChannelId.value"
            :can-manage-channels="waddles.canManageChannels.value"
            :can-manage-community="waddles.canManageCommunity.value"
            :is-loading="waddles.isLoadingStructure.value"
            :member-count="displayedMemberCount"
            :member-state="displayedMemberState"
            :active-channel-jids="messaging.activeChannels.value"
            :collapsed-group-ids="ui.collapsedSpaceGroupIds.value"
            :channel-unread-map="computedChannelUnreadMap"
            :call-participant-counts="callParticipantCounts"
            :call-participants="callParticipants"
            :call-media-by-room="callMediaByRoom"
            :managed-muc-domain="managedMucDomain"
            :thread-entries-fn="(roomJid: string) => channelUnread.threadEntries(roomJid)"
            :active-community-surface="ui.activeCommunitySurface.value"
            :stories-active-count="stories.activeStories.value.length"
            :upcoming-event-count="communityEvents.events.value.filter((event) => isEventUpcomingOrOngoing(event)).length"
            :is-threads-active="ui.activePage.value === 'threads'"
            :is-unread-active="ui.activePage.value === 'unread'"
            :unread-total-count="channelUnread.totalUnreadCount.value + channelUnread.totalThreadUnreadCount.value"
            @select-channel="selectChannelFromPage"
            @join-channel-call="joinChannelCallFromPage"
            @leave-channel-call="leaveChannelCallFromPage"
            @select-thread="onSelectThread"
            @select-community-surface="selectCommunitySurface"
            @select-threads-view="openThreads"
            @select-unread-view="openUnread"
            @create-channel="openCreateChannelDialog()"
            @create-channel-in-space="openCreateChannelDialog"
            @open-settings="ui.showWaddleSettings.value = true"
            @open-members="ui.showMembers.value = true"
            @update-collapsed-group-ids="ui.collapsedSpaceGroupIds.value = $event"
          />
        </div>
      </section>

      <section class="community-section" aria-label="Direct messages">
        <h2 class="community-section__title">Direct messages</h2>
        <div class="community-embed">
          <DmPanel
            :conversations="dmConversations.conversations.value"
            :group-dms="groupDmConversations"
            :active-peer-jid="dmConversations.activePeerJid.value"
            :active-group-dm-room-jid="null"
            :thread-entries="dmThreadEntries ?? []"
            :self-full-jid="selfFullJid"
            hide-current-call
            @answer-dm="answerDmFromPage"
            @select-dm="selectDm"
            @select-group-dm="selectGroupDm"
            @select-thread="openThread"
            @reconnect-dm="reconnectDmFromPage"
            @end-dm="endDmFromPage"
            @new-dm="ui.showNewDm.value = true"
            @new-group-dm="handleNewGroupDm"
            @add-people-to-dm="handleAddPeopleToDm"
          />
        </div>
      </section>
    </div>
  </div>
</template>
