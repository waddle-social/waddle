<script setup lang="ts">
import { computed } from "vue";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { avatarStore } from "@/lib/avatars/avatar-store";
import { isBotJid } from "@/lib/avatars/author-jid";
import { useAvatarUrl } from "@/lib/avatars/use-avatar-url";
import type { OccupantPresence } from "@/lib/xmpp-client";

/**
 * A person's avatar, by bare JID. Reads the shared avatar store (lazily
 * fetching on first render) and falls back to name-derived initials when
 * the JID is unknown or has no avatar.
 */
const props = defineProps<{
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
  inCall?: boolean;
  huddle?: boolean;
  speaking?: boolean;
}>();

// A bot's room presence only says when it last posted: never show a status.
const bot = computed(() => isBotJid(props.jid));
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
    :presence="bot ? undefined : presence"
    :last-seen="bot ? undefined : lastSeen"
    :in-call="inCall"
    :huddle="huddle"
    :speaking="speaking"
  />
</template>
