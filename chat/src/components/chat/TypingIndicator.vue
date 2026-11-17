<script setup lang="ts">
import UserAvatar from "@/components/ui/UserAvatar.vue";
import type { OccupantPresence } from "@/lib/xmpp-client";
import { css, cx } from "styled-system/css";
import { typing, waddle } from "styled-system/recipes";

defineProps<{
  typingUsers: string[];
  /** Real bare JID behind a typing nick, for the avatar; `null` = initials. */
  avatarJidFor: (nick: string) => string | null;
  roomPresence: Record<string, OccupantPresence>;
  /** social = top-pinned feed (bordered), chat = classic bottom composer. */
  variant: "social" | "chat";
}>();

// The faces sit in the message avatar column so the waddle lines up with bodies.
const rowClass = css({ flex: "none", display: "flex", alignItems: "center", gap: "3", px: "6", py: "1" });
const socialClass = css({ borderBottom: "1px solid {colors.border.soft}", py: "2" });
const facesClass = css({ display: "flex", justifyContent: "flex-end", width: "msgAvatar", flex: "none", "& > * + *": { ml: "-6px" } });
</script>

<template>
  <div :class="cx(rowClass, variant === 'social' && socialClass)">
    <span :class="facesClass" aria-hidden="true">
      <UserAvatar
        v-for="nick in typingUsers.slice(0, 3)"
        :key="`typing-avatar:${variant}:${nick}`"
        :name="nick"
        :jid="avatarJidFor(nick)"
        :presence="roomPresence[nick] ?? 'offline'"
        size="xs"
      />
    </span>
    <span :class="typing()">
      <span :class="waddle()" aria-hidden="true"><i /><i /><i /></span>
      <span>
        <template v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</template>
        <template v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</template>
        <template v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</template>
      </span>
    </span>
  </div>
</template>
