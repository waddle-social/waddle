<script setup lang="ts">
import { computed } from "vue";
import { Hash, Menu, MessagesSquare } from "lucide-vue-next";
import spot from "@waddle/design/assets/spot-empty.svg?url";
import { css, cx } from "styled-system/css";
import { badge, btn, empty } from "styled-system/recipes";
import { isForumChannel } from "@/lib/channel-types";
import { pageClasses as page } from "@/components/community/pages/page-classes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { useCallParticipantAvatarJid } from "@/lib/avatars/use-call-participant-avatar";
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
  handleAddPeopleToDm,
} = props.controller;

const callParticipantAvatarJid = useCallParticipantAvatarJid();

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

function tileAvatars(tile: RoomTile): { nick: string; jid: string | null }[] {
  return tile.nicks.slice(0, 4).map((nick) => ({
    nick,
    jid: callParticipantAvatarJid(tile.roomJid, nick),
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

// Rooms-Page board: one bordered list of rooms (live huddles first) with
// Open, or Join for a live huddle; the legacy room and DM panels follow
// as embedded cards.
const newRoomBtn = btn({ variant: "primary" });
const desktopOnly = css({ display: "none", md: { display: "inline-flex" } });
const newRoomSmBtn = btn({ variant: "primary", size: "sm" });
const openBtn = btn({ size: "sm" });
const joinBtn = btn({ variant: "live", size: "sm" });
const mentionBadge = badge();
const unreadBadge = badge({ variant: "quiet" });
const emptyClass = cx(empty(), css({ pt: "16", "& h2": { margin: 0, font: "700 24px/1.25 {fonts.display}", color: "fg" } }));
const loadingClass = css({ py: "6", textAlign: "center", font: "400 14px/1.45 {fonts.body}", color: "fg.muted" });
const list = css({ margin: 0, padding: 0, listStyle: "none", borderRadius: "16px", border: "1px solid {colors.border.soft}", bg: "surface", overflow: "hidden", "--ring-bg": "{colors.surface}" });
const row = css({
  display: "flex",
  alignItems: "center",
  gap: "3",
  minHeight: "64px",
  padding: "{spacing.3} {spacing.4}",
  borderBottom: "1px solid {colors.border.soft}",
  _last: { borderBottom: 0 },
  "&[data-live]": { bg: "live.subtle" },
  _narrow: { gap: "2.5", px: "3" },
});
const rowIcon = css({ width: "18px", height: "18px", flex: "none", color: "fg.muted" });
const rowEnter = css({
  display: "flex",
  flexDirection: "column",
  gap: "0.5",
  flex: 1,
  minWidth: 0,
  padding: 0,
  border: 0,
  bg: "transparent",
  color: "fg",
  font: "inherit",
  textAlign: "left",
  cursor: "pointer",
  "&:hover > :first-child": { textDecoration: "underline", textUnderlineOffset: "3px" },
});
const rowName = css({ font: "700 15px/1.3 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const rowMeta = css({ font: "400 13px/1.35 {fonts.body}", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const liveMeta = css({ color: "live.text" });
const rowSide = css({ display: "flex", alignItems: "center", gap: "2", flex: "none" });
const avatars = css({ display: "flex", "& > * + *": { ml: "-6px" }, _narrow: { display: "none" } });
const section = css({ display: "flex", flexDirection: "column", gap: "3" });
const embed = css({
  display: "flex",
  minHeight: "20rem",
  maxHeight: "44rem",
  overflow: "hidden",
  border: "1px solid {colors.border.soft}",
  borderRadius: "16px",
  bg: "surface",
  "& > *": { width: "100% !important", flex: "1 1 0% !important", borderRight: "0 !important" },
});
</script>

<template>
  <div :class="page.root">
    <!-- Phone header: the rail bar with the sidebar button. -->
    <header :class="page.mobileBar" data-theme="night">
      <button type="button" :class="page.mobileNav" aria-label="Open navigation" @click="emit('openNav')">
        <Menu aria-hidden="true" />
      </button>
      <b :class="page.mobileTitle">Browse rooms</b>
      <button v-if="waddles.canManageChannels.value" type="button" :class="newRoomSmBtn" @click="openCreateChannelDialog()">
        New room
      </button>
    </header>

    <div :class="page.scroll">
      <div :class="page.content">
        <header :class="page.head">
          <div :class="page.heading">
            <span :class="liveCount > 0 ? page.liveKicker : page.kicker">
              <span v-if="liveCount > 0" :class="page.ember" aria-hidden="true" />
              {{ tiles.length }} room{{ tiles.length === 1 ? "" : "s" }}<template v-if="liveCount > 0"> · {{ liveCount }} live</template>
            </span>
            <h1 :class="page.h1">Browse rooms</h1>
            <p :class="page.lead">Sorted by what is happening: huddles first, then mentions and unread.</p>
          </div>
          <button v-if="waddles.canManageChannels.value && tiles.length > 0" type="button" :class="[newRoomBtn, desktopOnly]" @click="openCreateChannelDialog()">
            New room
          </button>
        </header>

        <section aria-label="Rooms" :class="section">
          <div v-if="waddles.isLoadingStructure.value && tiles.length === 0" :class="loadingClass" role="status">
            Finding your rooms.
          </div>
          <div v-else-if="tiles.length === 0" :class="emptyClass" role="status">
            <img :src="spot" alt="" />
            <h2>No rooms yet</h2>
            <p v-if="waddles.canManageChannels.value">Create the first one to get people talking.</p>
            <p v-else>They show up here once a server owner creates one.</p>
            <button v-if="waddles.canManageChannels.value" type="button" :class="newRoomBtn" @click="openCreateChannelDialog()">
              New room
            </button>
          </div>
          <ul v-else :class="list">
            <li v-for="tile in tiles" :key="tile.channel.id" :class="row" :data-live="tile.live ? '' : undefined">
              <MessagesSquare v-if="isForumChannel(tile.channel)" :class="rowIcon" aria-hidden="true" />
              <Hash v-else :class="rowIcon" aria-hidden="true" />
              <button
                type="button"
                :class="rowEnter"
                :aria-label="`Enter ${tile.channel.name}`"
                @click="enterRoom(tile)"
              >
                <span :class="rowName">{{ tile.channel.name }}</span>
                <span :class="[rowMeta, tile.live && liveMeta]">{{ tile.kicker }}<template v-if="tile.preview"> · {{ tile.preview }}</template></span>
              </button>
              <div :class="rowSide">
                <div v-if="tile.nicks.length > 0" :class="avatars" :aria-label="`${tile.nicks.length} in the huddle`">
                  <UserAvatar v-for="avatar in tileAvatars(tile)" :key="avatar.nick" :name="avatar.nick" :jid="avatar.jid" size="xs" :in-call="false" />
                </div>
                <span v-if="tile.mentions > 0" :class="mentionBadge" :aria-label="`${tile.mentions} mentions`">@{{ tile.mentions }}</span>
                <span v-else-if="tile.unread > 0" :class="unreadBadge" :aria-label="`${tile.unread} unread`">{{ tile.unread }}</span>
                <button
                  v-if="tile.live"
                  type="button"
                  :class="joinBtn"
                  :aria-label="`Join the huddle in ${tile.channel.name}`"
                  @click="joinHuddle(tile)"
                >
                  Join
                </button>
                <button
                  v-else
                  type="button"
                  :class="openBtn"
                  :aria-label="`Enter ${tile.channel.name}`"
                  @click="enterRoom(tile)"
                >
                  Open
                </button>
              </div>
            </li>
          </ul>
        </section>

        <section :class="section" aria-label="Browse all rooms">
          <h2 :class="page.sectionTitle">Browse all rooms</h2>
          <div :class="embed">
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

        <section :class="section" aria-label="Direct messages">
          <h2 :class="page.sectionTitle">Direct messages</h2>
          <div :class="embed">
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
              @add-people-to-dm="handleAddPeopleToDm"
            />
          </div>
        </section>
      </div>
    </div>
  </div>
</template>
