<script setup lang="ts">
import { computed } from "vue";
import { CheckCheck, Phone, Video } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { badge, btn } from "styled-system/recipes";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import { jidLocalpart } from "@/lib/xmpp/jid";
import type { CallMedia } from "@/lib/calls/types";
import { threadDisplayTitle } from "@/lib/threads-view-filters";
import { useCallAnchorCardState, wasmThreadEntryToAnchorMessage } from "@/lib/call-thread-anchor";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import CallAnchorCard from "@/components/calls/CallAnchorCard.vue";

const props = defineProps<{
  entry: WasmThreadEntry;
  markingRead?: boolean;
  /** Real bare JID of the thread starter when its root row is loaded; `null` = initials. */
  rootAuthorJid?: string | null;
}>();

const emit = defineEmits<{
  open: [entry: WasmThreadEntry];
  markRead: [entry: WasmThreadEntry];
  joinCall: [entry: WasmThreadEntry, media: CallMedia];
}>();

const unreadCountClass = badge();
// Phones keep the action as a 44px icon button so the title has room.
const markReadClass = cx(btn({ size: "sm" }), css({ flex: "none", "& svg": { display: "none" }, mdDown: { width: "44px", height: "44px", px: "0", justifyContent: "center", "& svg": { display: "block" } } }));
const markReadLabelClass = css({ mdDown: { srOnly: true } });

// Disc-List board row: avatar, a one-line title (bold while unread), a
// "#room · replies · when" line and the gold unread count.
const rowClass = css({
  display: "flex",
  alignItems: "center",
  gap: "3",
  padding: "0 {spacing.4} 0 0",
  transition: "background-color {durations.fast} {easings.out}",
  _hover: { bg: "state.hover" },
});
const callOpenClass = css({ flex: 1, minWidth: 0, padding: "{spacing.3} 0 {spacing.3} {spacing.4}", cursor: "pointer" });
const openClass = css({
  display: "flex",
  flex: 1,
  minWidth: 0,
  alignItems: "center",
  gap: "3",
  padding: "{spacing.3} 0 {spacing.3} {spacing.4}",
  border: 0,
  bg: "transparent",
  color: "fg",
  textAlign: "left",
  cursor: "pointer",
  _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "-3px", borderRadius: "control" },
});
const avatarClass = css({ display: "flex", flex: "none" });
const textClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5" });
const titleRowClass = css({ display: "flex", alignItems: "center", gap: "2", minWidth: 0 });
const titleClass = css({ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: "15px", lineHeight: "1.35" });
const titleUnreadClass = cx(titleClass, css({ fontWeight: 700 }));
const metaClass = css({ fontSize: "13px", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const callFlagClass = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  width: "20px",
  height: "20px",
  flex: "none",
  borderRadius: "pill",
  border: "1px solid {colors.live}",
  color: "live.text",
  "& svg": { width: "12px", height: "12px" },
});

// A MUC call-thread row shares the one live-state composable and the one
// Join path with the in-channel anchor card and the call banner, so the
// global Discussions view reflects the same live/ended call state. Rows
// that don't anchor a MUC call (DM anchors, plain threads) keep the card.
const anchorMessage = computed(() => wasmThreadEntryToAnchorMessage(props.entry));
const callState = useCallAnchorCardState(
  () => anchorMessage.value ?? { body: "", author: "", threadId: undefined, callThread: undefined },
  () => props.entry.channel,
  () => props.entry.reply_count,
);
const isCallThread = computed(() => callState.value !== null);

const recencyLabel = computed(() => {
  const ts = Date.parse(props.entry.last_activity);
  if (Number.isNaN(ts)) return "";
  const deltaSec = Math.floor((Date.now() - ts) / 1000);
  if (deltaSec < 60) return "just now";
  if (deltaSec < 3600) return `${Math.floor(deltaSec / 60)}m ago`;
  if (deltaSec < 86_400) return `${Math.floor(deltaSec / 3600)}h ago`;
  return `${Math.floor(deltaSec / 86_400)}d ago`;
});

const channelLabel = computed(() => `#${jidLocalpart(props.entry.channel)}`);

const title = computed(() => threadDisplayTitle(props.entry));

const replyLabel = computed(() => {
  const replies = props.entry.reply_count;
  return `${replies} ${replies === 1 ? "reply" : "replies"}`;
});

// The threads query carries only the person who started the discussion,
// so the avatar stack shows exactly that: no invented participants.
const rootAuthor = computed(() => props.entry.root_author?.trim() ?? "");
// `root_author` is a display label only (a room nick may have changed
// hands): the avatar comes from the loaded root row's resolved author.
const rootAuthorJid = computed(() => props.rootAuthorJid ?? null);

const isDmCallThread = computed(() => props.entry.callThread?.kind === "dm");
const dmCallFlagLabel = computed(() => {
  if (!isDmCallThread.value) return "";
  const hasVideo = props.entry.callThread?.media.includes("video") ?? false;
  return hasVideo ? "Video call thread" : "Call thread";
});
const DmCallFlagIcon = computed(() => {
  const hasVideo = props.entry.callThread?.media.includes("video") ?? false;
  return hasVideo ? Video : Phone;
});
</script>

<template>
  <article :class="rowClass">
    <div
      v-if="isCallThread && callState"
      class="call-thread-row__open"
      :class="callOpenClass"
      @click="emit('open', entry)"
    >
      <CallAnchorCard
        :state="callState"
        @join="callState && emit('joinCall', entry, callState.media)"
        @open-thread="emit('open', entry)"
      />
    </div>
    <button
      v-else
      type="button"
      :class="openClass"
      @click="emit('open', entry)"
    >
      <span v-if="rootAuthor" :class="avatarClass" aria-hidden="true">
        <UserAvatar :name="rootAuthor" :jid="rootAuthorJid" size="sm" />
      </span>
      <span :class="textClass">
        <span :class="titleRowClass">
          <span
            v-if="isDmCallThread"
            :class="callFlagClass"
            role="img"
            :aria-label="dmCallFlagLabel"
          >
            <component :is="DmCallFlagIcon" aria-hidden="true" />
          </span>
          <span :class="entry.has_unread ? titleUnreadClass : titleClass">{{ title }}</span>
        </span>
        <span :class="metaClass">
          <template v-if="rootAuthor">{{ rootAuthor }} · </template>{{ channelLabel }} · {{ replyLabel }}<template v-if="recencyLabel"> · {{ recencyLabel }}</template>
        </span>
      </span>
      <span
        v-if="entry.has_unread"
        :class="unreadCountClass"
        :aria-label="`${entry.unread} unread`"
      >{{ entry.unread }}</span>
    </button>
    <button
      v-if="entry.has_unread"
      type="button"
      :class="markReadClass"
      :disabled="props.markingRead"
      @click.stop="emit('markRead', entry)"
    >
      <CheckCheck aria-hidden="true" />
      <span :class="markReadLabelClass">{{ props.markingRead ? "Marking..." : "Mark read" }}</span>
    </button>
  </article>
</template>
