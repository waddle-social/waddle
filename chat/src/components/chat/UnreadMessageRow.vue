<script setup lang="ts">
import type { TimelineMessage } from "@/lib/chat-ui";
import { formatTimelineStamp } from "@/channels/timeline";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { authorAvatarJid } from "@/lib/avatars/author-jid";
import MessageBody from "@/components/chat/MessageBody.vue";
import { css, cx } from "styled-system/css";
import { msg } from "styled-system/recipes";

// Read-only message row for the Unread digest. Reuses `MessageBody`
// (the same presentational component `PinnedPanel` uses) so unread
// messages render their full bodies — markup, mentions, attachments,
// link previews — without any of the live-channel action wiring that
// `MessageCard` requires.
defineProps<{
  message: TimelineMessage;
}>();

// The board's `wd-msg` row, square-cornered inside its Inbox card.
const rowClass = cx(msg(), css({ borderRadius: 0, padding: "10px 16px", _hover: { bg: "transparent" } }));
const avatarClass = css({ gridRow: "span 2", alignSelf: "start", mt: "0.5" });
const retractedClass = css({ gridColumn: 2, margin: 0, textStyle: "caption", fontStyle: "italic", color: "fg.muted" });
const bodyClass = css({ gridColumn: 2, minWidth: 0 });
</script>

<template>
  <div :class="rowClass">
    <span :class="avatarClass">
      <UserAvatar :name="message.author" :jid="authorAvatarJid(message)" size="message" />
    </span>
    <div class="wd-msg__meta">
      <span class="wd-msg__author">{{ message.author }}</span>
      <span class="wd-msg__time">{{ formatTimelineStamp(message.createdAt) }}</span>
    </div>
    <p v-if="message.isRetracted" :class="retractedClass">Message retracted</p>
    <div v-else :class="bodyClass"><MessageBody :message="message" compact /></div>
  </div>
</template>
