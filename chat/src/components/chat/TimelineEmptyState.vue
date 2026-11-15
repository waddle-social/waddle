<script setup lang="ts">
import { computed } from "vue";
import { AlertTriangle, Hash, LockKeyhole, MessageCircle, MessagesSquare } from "lucide-vue-next";
import mark from "@waddle/design/assets/waddle-mark.svg?url";
import spot from "@waddle/design/assets/spot-conversation.svg?url";
import { css } from "styled-system/css";
import { empty } from "styled-system/recipes";

const props = defineProps<{
  /**
   * pick  → no conversation selected yet.
   * quiet → conversation selected but its timeline is empty.
   * access-required → the server denied this account access to the channel.
   * unavailable → the timeline failed to load (the error banner offers Retry).
   */
  variant: "pick" | "quiet" | "access-required" | "unavailable";
  sidebarMode?: "channels" | "dms";
  isForumChannel: boolean;
  dmPeerUsername?: string | null;
  channelName?: string | null;
}>();

/** Empty-Timeline / App-DMs boards: one illustration or icon, a title and one line. */
const state = computed(() => {
  switch (props.variant) {
    case "access-required":
      return { icon: LockKeyhole, title: "You need access to this room", body: "Ask a room admin for access, then open it again." };
    case "unavailable":
      return { icon: AlertTriangle, title: "Messages are not available right now", body: "Check your connection and try again." };
    case "pick":
      if (props.sidebarMode === "dms") {
        return { spot: true, title: "Pick a conversation", body: "Choose someone in the sidebar, or start a new one." };
      }
      return props.isForumChannel
        ? { icon: MessagesSquare, title: "Pick a forum", body: "Open one to browse its topics." }
        : { icon: Hash, title: "Pick a room", body: "Open a room from the sidebar to join the conversation." };
    default:
      if (props.dmPeerUsername) {
        return { icon: MessageCircle, title: `Just you and @${props.dmPeerUsername}`, body: "Send the first message to get the conversation going." };
      }
      return props.isForumChannel
        ? { icon: MessagesSquare, title: `Welcome to #${props.channelName}`, body: "Start the first topic with a clear title so people can follow the thread." }
        : { mascot: true, title: `It's quiet in #${props.channelName}`, body: "Say hello, share what you're working on, or ask the room something." };
  }
});

const rootClass = css({ minHeight: "100%", justifyContent: "center", maxWidth: "520px" });
const mascotClass = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  width: "120px",
  height: "120px",
  borderRadius: "pill",
  bg: "belly",
  "& img": { width: "88px", height: "88px" },
});
const iconClass = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  width: "80px",
  height: "80px",
  borderRadius: "pill",
  bg: "action.subtle",
  color: "action.text",
  "& svg": { width: "34px", height: "34px" },
});
const titleClass = css({ margin: 0, font: "700 24px/1.2 {fonts.display}", color: "fg", textWrap: "balance" });
</script>

<template>
  <div
    :class="[empty(), rootClass]"
    role="status"
    :aria-live="variant === 'access-required' ? 'polite' : undefined"
  >
    <img v-if="state.spot" :src="spot" alt="" />
    <span v-else-if="state.mascot" :class="mascotClass" aria-hidden="true">
      <img :src="mark" alt="" />
    </span>
    <span v-else :class="iconClass" aria-hidden="true">
      <component :is="state.icon" :stroke-width="1.75" />
    </span>
    <h2 :class="titleClass">{{ state.title }}</h2>
    <p>{{ state.body }}</p>
  </div>
</template>
