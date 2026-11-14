<script setup lang="ts">
import { css } from "styled-system/css";
import { liveDot } from "styled-system/recipes";
import { computed } from "vue";
import { Phone, Video } from "lucide-vue-next";
import { useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";

/**
 * Live-call indicator shown in the channel header when the room owns
 * an active MUC call AND the local user is NOT currently in it.
 *
 * Replaces the previous in-button chip inside `MucCallButton` so the
 * "join" affordance now reads as a single statement: "a call is
 * happening, click to join". Muji presence proves that a call is live
 * in the room, but it does not carry a protocol-backed start time, so
 * this pill intentionally avoids rendering elapsed duration.
 *
 * Hides itself once the local user joins (the split-view surface
 * communicates the state from then on) and when the call ends.
 */
const props = defineProps<{
  /** Channel's bare MUC JID — `room@muc.host`. */
  roomJid: string;
  /** Click handler that initiates a join. Owned by the parent
   *  (typically `MucCallButton`) so all of the call-start plumbing
   *  (sender lookup, media defaults, busy guards) stays in one place. */
  onJoin: () => void;
  /** Disable the join action — set when the local user is already in
   *  a different call, or when the engine is mid-start. */
  disabled?: boolean;
}>();

const {
  hasActiveCall,
  selfInCall,
  localResourceInCall,
  participantCount,
  media,
} = useRoomHasActiveCall(
  () => props.roomJid,
);

/**
 * The pill is visible only when:
 *   - There is an active MUC call in THIS room per Muji presence.
 *   - This browser resource is not currently joined to it (otherwise
 *     the split surface is already painting the state for them).
 */
const isVisible = computed(() => hasActiveCall.value && !localResourceInCall.value);

const ariaLabel = computed(() => {
  const count = participantCount.value;
  const noun = count === 1 ? "person" : "people";
  const mediaLabel = media.value.video ? "video call" : "call";
  const device = selfInCall.value
    ? " This account is connected on another device."
    : "";
  if (props.disabled) {
    return `Live ${mediaLabel} in this channel, ${count} ${noun}.${device} Join unavailable from this device.`;
  }
  return `Live ${mediaLabel} in this channel, ${count} ${noun}.${device} Click to join from this device.`;
});

function onClick(): void {
  if (props.disabled) return;
  props.onJoin();
}

// Live pill in a room header: someone is in a huddle here and you are not.
const pillClass = css({
  display: "inline-flex",
  alignItems: "center",
  gap: "1.5",
  height: "32px",
  px: "3",
  borderRadius: "pill",
  border: "1px solid {colors.live}",
  bg: "live.subtle",
  color: "live.text",
  font: "600 13px/1 {fonts.body}",
  fontVariantNumeric: "tabular-nums",
  cursor: "pointer",
  transition: "background-color {durations.fast} {easings.out}",
  _hover: { bg: "live", color: "live.fg" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
  _disabled: { opacity: 0.55, cursor: "not-allowed", _hover: { bg: "live.subtle", color: "live.text" } },
  _coarse: { height: "44px" },
  "& svg": { width: "14px", height: "14px" },
});
const separatorClass = css({ opacity: 0.5 });
</script>

<template>
  <button
    v-if="isVisible"
    type="button"
    :class="pillClass"
    :disabled="disabled"
    :aria-label="ariaLabel"
    :title="ariaLabel"
    @click="onClick"
  >
    <span :class="liveDot()" aria-hidden="true" />
    <Video v-if="media.video" aria-hidden="true" />
    <Phone v-else aria-hidden="true" />
    <span>{{ participantCount }}</span>
    <span :class="separatorClass" aria-hidden="true">·</span>
    <span>Live</span>
  </button>
</template>
