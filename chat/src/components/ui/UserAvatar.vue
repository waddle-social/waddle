<script setup lang="ts">
import AppAvatar from "@/components/ui/AppAvatar.vue";
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
  size?: "xs" | "sm" | "md" | "lg" | "xl" | "message";
  presence?: OccupantPresence;
  lastSeen?: number;
  inCall?: boolean;
  huddle?: boolean;
  speaking?: boolean;
}>();

const src = useAvatarUrl(() => props.jid);
</script>

<template>
  <AppAvatar
    :name="name"
    :src="src"
    :size="size"
    :presence="presence"
    :last-seen="lastSeen"
    :in-call="inCall"
    :huddle="huddle"
    :speaking="speaking"
  />
</template>
