<script setup lang="ts">
import { computed } from "vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { avatarStore } from "@/lib/avatars/avatar-store";
import { isBotJid } from "@/lib/avatars/author-jid";
import { useAvatarUrl } from "@/lib/avatars/use-avatar-url";
import { presenceFromShow, useLivePresence } from "@/presence/live-presence";
import { useInCallOverlay } from "@/presence/in-call-overlay-store";
import type { OccupantPresence } from "@/lib/xmpp-client";

/**
 * A person's avatar, by bare JID. Reads the shared avatar store (lazily
 * fetching on first render) and falls back to name-derived initials when
 * the JID is unknown or has no avatar.
 */
const props = withDefaults(defineProps<{
  jid?: string | null;
  name: string;
  /**
   * Shown only while the store has not resolved `jid` (e.g. the sign-in
   * provider's picture); a known "no avatar" still renders initials.
   */
  fallbackSrc?: string | null;
  size?: "xs" | "sm" | "md" | "lg" | "xl" | "message";
  presence?: OccupantPresence;
  lastSeen?: number;
  /** Omit to follow the person's in-call overlay; `false` hides the badge
   * where it says nothing new (a call's own participant list). */
  inCall?: boolean;
  huddle?: boolean;
  speaking?: boolean;
}>(), { inCall: undefined });

// A bot's room presence only says when it last posted: never show a status.
const bot = computed(() => isBotJid(props.jid));
// What the app knows about the person wins over the caller's guess (often a
// focused-room nick map, which says "offline" for anyone outside that room).
const live = useLivePresence(() => props.jid);
const shownPresence = computed(() => (bot.value ? undefined : presenceFromShow(live.value) ?? props.presence));
const overlayInCall = useInCallOverlay(() => props.jid);
const shownInCall = computed(() => props.inCall ?? overlayInCall.value);
const storeSrc = useAvatarUrl(() => props.jid);
const src = computed(() => {
  if (storeSrc.value) return storeSrc.value;
  if (avatarStore.isKnownAbsent(props.jid)) return null;
  return props.fallbackSrc ?? null;
});
</script>

<template>
  <AppAvatar
    :name="name"
    :src="src"
    :size="size"
    :presence="shownPresence"
    :last-seen="bot ? undefined : lastSeen"
    :in-call="shownInCall"
    :huddle="huddle"
    :speaking="speaking"
  />
</template>
