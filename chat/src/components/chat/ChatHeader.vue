<script setup lang="ts">
import { computed, type Component } from "vue";
import { Hash, Menu, MessageCircle, MessagesSquare, Pin, Search, Settings, Users } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ConnectionNoticeCopy } from "@/lib/connection-notice";
import type { OccupantPresence } from "@/lib/xmpp-client";
import type { MemberLoadState } from "@/waddles/directory";

interface ConnectionStatusClasses {
  banner: string;
  iconWrap: string;
  chip: string;
  body: string;
}

export interface ChannelHeaderMember {
  nick: string;
  jid?: string;
  avatarUrl?: string | null;
  presence: OccupantPresence;
}

const props = defineProps<{
  waddle: SpaceSummary | null;
  channel: ChannelSummary | null;
  dmPeer?: { peerJid?: string; peerUsername: string; presenceShow?: string } | null;
  isForumChannel: boolean;
  canManageChannels: boolean;
  memberCount: number | null;
  memberState: MemberLoadState;
  /** Roster of room occupants with their current presence, used to render
   * the live presence avatar stack on the right of the header. Sorted
   * online-first by the caller. */
  members?: ChannelHeaderMember[];
  connectionNotice: ConnectionNoticeCopy | null;
  connectionStatusClasses: ConnectionStatusClasses | null;
  connectionStatusIcon: Component;
}>();

const MAX_HEADER_AVATARS = 4;

const sortedMembers = computed<ChannelHeaderMember[]>(() => {
  const list = props.members ?? [];
  const rank = (p: OccupantPresence) => {
    switch (p) {
      case "online": return 0;
      case "dnd":    return 1;
      case "away":   return 2;
      default:       return 3;
    }
  };
  return [...list].sort((a, b) => {
    const r = rank(a.presence) - rank(b.presence);
    if (r !== 0) return r;
    return a.nick.localeCompare(b.nick);
  });
});

const visibleMembers = computed(() => sortedMembers.value.slice(0, MAX_HEADER_AVATARS));
const overflowCount = computed(() => Math.max(0, sortedMembers.value.length - visibleMembers.value.length));
const onlineCount = computed(() => sortedMembers.value.filter((m) => m.presence === "online").length);

const showSearch = defineModel<boolean>("showSearch", { required: true });
const showPinnedPanel = defineModel<boolean>("showPinnedPanel", { default: false });

const emit = defineEmits<{
  openNav: [];
  openDetails: [];
  editChannel: [];
  selectMember: [jid: string];
}>();

function presenceLabel(p: OccupantPresence): string {
  switch (p) {
    case "online": return "online";
    case "away":   return "away";
    case "dnd":    return "do not disturb";
    default:       return "offline";
  }
}

function onAvatarClick(member: ChannelHeaderMember) {
  if (!member.jid) {
    emit("openDetails");
    return;
  }
  emit("selectMember", member.jid);
}

function presenceText(show?: string): string {
  if (show === "available") return "online";
  if (show === "away") return "away";
  if (show === "dnd") return "do not disturb";
  if (show === "xa") return "extended away";
  return "offline";
}

const memberButtonCopy = computed(() => {
  if (props.memberCount !== null) {
    const noun = props.memberCount === 1 ? "member" : "members";
    const suffix = props.memberState === "unavailable" ? ", last synced" : "";
    return {
      title: `${props.memberCount} ${noun}${suffix}`,
      aria: `Open details (${props.memberCount} ${noun}${suffix})`,
      primary: String(props.memberCount),
      secondary: noun,
    };
  }

  if (props.memberState === "loading") {
    return {
      title: "Members syncing",
      aria: "Open details (members syncing)",
      primary: "Syncing",
      secondary: "members",
    };
  }

  return {
    title: "Members unavailable",
    aria: "Open details (members unavailable)",
    primary: "Unavailable",
    secondary: "members",
  };
});
</script>

<template>
  <div class="chat-pane-header border-b border-border px-[var(--chat-content-inline)] py-0 flex flex-shrink-0 items-center justify-between gap-[var(--space-md)] glass-surface">
    <div class="chat-message-lane flex min-w-0 items-center gap-2">
      <button
        class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground lg:hidden"
        type="button"
        aria-label="Open navigation"
        @click="emit('openNav')"
      >
        <Menu class="w-4 h-4" />
      </button>
      <div class="chat-pane-title-group">
        <span class="chat-pane-title-icon rounded-lg bg-primary/8">
          <component :is="dmPeer ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-4 h-4 text-primary/70" />
        </span>
        <div class="min-w-0">
          <div class="flex min-w-0 items-center gap-2">
            <h1 class="type-chat-title truncate">
              {{ dmPeer ? dmPeer.peerUsername : channel?.name ?? "…" }}
            </h1>
            <span
              v-if="isForumChannel"
              class="type-badge rounded-full border border-primary/15 bg-primary/8 px-2 py-0.5 text-primary/80"
            >
              Forum
            </span>
            <span v-if="dmPeer" class="hidden lg:inline type-meta text-muted-foreground">
              · {{ presenceText(dmPeer.presenceShow) }}
            </span>
          </div>
          <div v-if="dmPeer" class="lg:hidden type-caption text-muted-foreground truncate">
            {{ presenceText(dmPeer.presenceShow) }}
          </div>
          <div v-else-if="channel && waddle" class="lg:hidden type-caption text-muted-foreground truncate">
            {{ waddle.name }}
          </div>
        </div>
      </div>
    </div>
    <div class="flex shrink-0 items-center gap-3">
      <div
        v-if="connectionNotice && connectionStatusClasses"
        class="type-caption type-emphasis hidden md:inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1"
        :class="connectionStatusClasses.chip"
      >
        <component
          :is="connectionStatusIcon"
          class="w-3.5 h-3.5"
          :class="{ 'motion-safe:animate-spin': connectionNotice.tone === 'reconnecting' }"
        />
        <span>{{ connectionNotice.shortLabel }}</span>
      </div>
      <div class="flex items-center gap-1.5">
        <button
          v-if="channel || dmPeer"
          class="chat-icon-button chat-icon-button--md transition-all duration-200"
          :class="showSearch ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          title="Search messages"
          aria-label="Search messages"
          :aria-pressed="showSearch"
          type="button"
          @click="showSearch = !showSearch"
        >
          <Search class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel"
          class="chat-icon-button chat-icon-button--md transition-all duration-200"
          :class="showPinnedPanel ? 'bg-muted text-primary' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          title="Pinned messages"
          aria-label="Pinned messages"
          :aria-pressed="showPinnedPanel"
          type="button"
          @click="showPinnedPanel = !showPinnedPanel"
        >
          <Pin class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="canManageChannels && channel"
          class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground"
          title="Channel settings"
          aria-label="Channel settings"
          type="button"
          @click="emit('editChannel')"
        >
          <Settings class="w-3.5 h-3.5" />
        </button>
        <!-- Live presence stack: at-a-glance "who's here right now" instead
             of a static "N members" count. Replaces the eye's first read
             of "this is a room labeled #x" with "this is a room with
             these specific people present right now." -->
        <button
          v-if="channel"
          class="chat-presence-stack"
          type="button"
          :title="memberButtonCopy.title"
          :aria-label="`${memberButtonCopy.aria}. ${onlineCount} online.`"
          @click="emit('openDetails')"
        >
          <span
            v-if="visibleMembers.length > 0"
            class="chat-presence-stack__avatars"
            @click.stop
          >
            <span
              v-for="member in visibleMembers"
              :key="`hdr-avatar:${member.nick}`"
              class="chat-presence-stack__avatar-wrap"
              :class="`chat-presence-stack__avatar-wrap--${member.presence}`"
              :title="`${member.nick} · ${presenceLabel(member.presence)}`"
              tabindex="0"
              @click.stop="onAvatarClick(member)"
              @keydown.enter.stop="onAvatarClick(member)"
            >
              <AppAvatar
                :name="member.nick"
                :src="member.avatarUrl ?? null"
                :presence="member.presence"
                size="xs"
              />
            </span>
          </span>
          <span
            v-if="overflowCount > 0"
            class="chat-presence-stack__overflow"
            :title="`${overflowCount} more`"
          >+{{ overflowCount }}</span>
          <span v-else-if="visibleMembers.length === 0" class="chat-presence-stack__fallback">
            <Users class="w-3.5 h-3.5" />
            <span class="type-control">{{ memberButtonCopy.primary }}</span>
          </span>
        </button>
      </div>
    </div>
  </div>
</template>
