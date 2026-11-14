<script setup lang="ts">
import type { TimelineMessage } from "@/lib/chat-ui";
import { formatTimelineStamp } from "@/channels/timeline";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { authorAvatarJid } from "@/lib/avatars/author-jid";
import MessageBody from "@/components/chat/MessageBody.vue";
import { css } from "styled-system/css";

// Read-only message row for the Unread digest. Reuses `MessageBody`
// (the same presentational component `PinnedPanel` uses) so unread
// messages render their full bodies — markup, mentions, attachments,
// link previews — without any of the live-channel action wiring that
// `MessageCard` requires.
defineProps<{
  message: TimelineMessage;
}>();

const row = css({ display: "flex", gap: "2.5", borderRadius: "control", p: "2" });
const avatarCell = css({ mt: "0.5", flex: "none" });
const content = css({ minWidth: 0, flex: 1 });
const meta = css({ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: "2" });
const author = css({ textStyle: "author", color: "fg", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const time = css({ flex: "none", textStyle: "timestamp", color: "fg.muted", fontVariantNumeric: "tabular-nums" });
const retracted = css({ textStyle: "caption", fontStyle: "italic", color: "fg.muted" });
</script>

<template>
  <div :class="row">
    <span :class="avatarCell">
      <UserAvatar :name="message.author" :jid="authorAvatarJid(message)" size="sm" />
    </span>
    <div :class="content">
      <div :class="meta">
        <span :class="author">{{ message.author }}</span>
        <span :class="time">
          {{ formatTimelineStamp(message.createdAt) }}
        </span>
      </div>
      <p
        v-if="message.isRetracted"
        :class="retracted"
      >Message retracted</p>
      <MessageBody v-else :message="message" compact />
    </div>
  </div>
</template>
