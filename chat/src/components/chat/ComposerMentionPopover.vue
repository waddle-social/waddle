<script setup lang="ts">
import { css } from "styled-system/css";
import { composerPopover } from "./composer-classes";
import { Megaphone, Radio } from "lucide-vue-next";
import type { MentionCandidate } from "@/lib/mentions";
import { useAvatarUrls } from "@/lib/avatars/use-avatar-url";

const props = defineProps<{
  results: MentionCandidate[];
  selectedIndex: number;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  pick: [candidate: MentionCandidate];
}>();

const mutedGlyph = css({ bg: "surface.2", color: "fg.muted" });
const nameClass = css({ fontWeight: 700 });

const avatarUrlFor = useAvatarUrls(() => props.results.map((candidate) => candidate.jid));
</script>

<template>
  <!-- @mention autocomplete -->
  <div
    :class="[composerPopover.root, isTopPinned ? composerPopover.below : composerPopover.above]"
  >
      <button
        v-for="(candidate, i) in results"
        :key="candidate.kind === 'broadcast' ? `broadcast:${candidate.username}` : candidate.jid ?? candidate.username"
        type="button"
        :class="composerPopover.option"
        :data-selected="i === selectedIndex || undefined"
        @mousedown.prevent="emit('pick', candidate)"
      >
        <!-- Broadcast mentions get distinct glyphs so @everyone vs @here
             reads at a glance without scanning the username text:
             Megaphone = announce to everyone, Radio = ping the people
             who are tuned in (online here). Other broadcast values
             (future-proof) fall back to a generic @ mark. -->
        <span
          v-if="candidate.kind === 'broadcast' && candidate.username === 'everyone'"
          :class="composerPopover.glyph"
        >
          <Megaphone aria-hidden="true" />
        </span>
        <span
          v-else-if="candidate.kind === 'broadcast' && candidate.username === 'here'"
          :class="composerPopover.glyph"
        >
          <Radio aria-hidden="true" />
        </span>
        <span
          v-else-if="candidate.kind === 'broadcast'"
          :class="composerPopover.glyph"
        >@</span>
        <img
          v-else-if="avatarUrlFor(candidate.jid)"
          :src="avatarUrlFor(candidate.jid) ?? undefined"
          :alt="candidate.username"
          :class="composerPopover.glyph"
          loading="lazy"
        />
        <span
          v-else
          :class="[composerPopover.glyph, mutedGlyph]"
        >@</span>
        <span :class="nameClass">{{ candidate.username }}</span>
      </button>
  </div>
</template>
