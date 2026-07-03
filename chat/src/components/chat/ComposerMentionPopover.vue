<script setup lang="ts">
import { Megaphone, Radio } from "lucide-vue-next";
import type { MentionCandidate } from "@/lib/mentions";

defineProps<{
  results: MentionCandidate[];
  selectedIndex: number;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  pick: [candidate: MentionCandidate];
}>();
</script>

<template>
  <!-- @mention autocomplete -->
  <div
    class="z-popover chat-composer-popover absolute glass-panel border border-border rounded-lg max-h-56 overflow-auto min-w-0 shadow-xl animate-fade-in p-1"
    :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
  >
    <div class="flex flex-col gap-1">
      <button
        v-for="(candidate, i) in results"
        :key="candidate.kind === 'broadcast' ? `broadcast:${candidate.username}` : candidate.jid ?? candidate.username"
        type="button"
        class="type-control w-full h-9 px-3 py-0 text-left transition-colors flex items-center gap-2 rounded-lg"
        :class="i === selectedIndex
          ? 'bg-primary/15 hover:bg-primary/20'
          : 'hover:bg-muted'"
        @mousedown.prevent="emit('pick', candidate)"
      >
        <!-- Broadcast mentions get distinct glyphs so @everyone vs @here
             reads at a glance without scanning the username text:
             Megaphone = announce to everyone, Radio = ping the people
             who are tuned in (online here). Other broadcast values
             (future-proof) fall back to a generic @ mark. -->
        <span
          v-if="candidate.kind === 'broadcast' && candidate.username === 'everyone'"
          class="flex h-5 w-5 items-center justify-center rounded bg-primary/10 text-primary"
        >
          <Megaphone class="h-3 w-3" aria-hidden="true" />
        </span>
        <span
          v-else-if="candidate.kind === 'broadcast' && candidate.username === 'here'"
          class="flex h-5 w-5 items-center justify-center rounded bg-primary/10 text-primary"
        >
          <Radio class="h-3 w-3" aria-hidden="true" />
        </span>
        <span
          v-else-if="candidate.kind === 'broadcast'"
          class="flex h-5 w-5 items-center justify-center rounded bg-primary/10 text-primary type-caption"
        >@</span>
        <img
          v-else-if="candidate.avatar_url"
          :src="candidate.avatar_url"
          :alt="candidate.username"
          class="h-5 w-5 rounded object-cover bg-muted"
          loading="lazy"
        />
        <span
          v-else
          class="type-caption flex h-5 w-5 items-center justify-center rounded bg-muted text-muted-foreground"
        >@</span>
        <span class="type-emphasis">{{ candidate.username }}</span>
      </button>
    </div>
  </div>
</template>
