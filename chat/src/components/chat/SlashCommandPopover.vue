<script setup lang="ts">
import { Slash, AlertCircle } from "lucide-vue-next";
import type { SlashCandidate } from "@/lib/slash-candidates";

defineProps<{
  candidates: SlashCandidate[];
  selectedIndex: number;
  prefix: string;
  blocked: boolean;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  pick: [candidate: SlashCandidate];
}>();

function candidateKey(candidate: SlashCandidate): string {
  return candidate.kind === "builtin"
    ? `builtin:${candidate.command.name}`
    : `extension:${candidate.command.serviceJid}:${candidate.command.node}`;
}
</script>

<template>
  <div
    class="z-popover chat-composer-popover absolute glass-panel border border-border rounded-lg max-h-56 overflow-auto min-w-0 shadow-xl animate-fade-in p-1"
    :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
  >
    <div v-if="blocked" class="type-caption flex items-center gap-2 px-3 py-2 text-destructive-text">
      <AlertCircle class="h-3.5 w-3.5" aria-hidden="true" />
      <span>No command <span class="type-emphasis">/{{ prefix }}</span>. Press Esc, then Enter to send as text.</span>
    </div>
    <div v-else class="flex flex-col gap-1">
      <button
        v-for="(candidate, i) in candidates"
        :key="candidateKey(candidate)"
        type="button"
        class="type-control w-full h-9 px-3 py-0 text-left transition-colors flex items-center gap-2 rounded-lg"
        :class="i === selectedIndex
          ? 'bg-primary/15 hover:bg-primary/20'
          : 'hover:bg-muted'"
        @mousedown.prevent="emit('pick', candidate)"
      >
        <Slash class="h-4 w-4 shrink-0 text-primary" aria-hidden="true" />
        <template v-if="candidate.kind === 'builtin'">
          <span class="type-emphasis shrink-0">{{ candidate.command.usage }}</span>
          <span class="type-caption text-muted-foreground truncate">{{ candidate.command.description }}</span>
          <span class="type-caption ml-auto shrink-0 rounded border border-border px-1.5 text-muted-foreground">Built-in</span>
        </template>
        <template v-else>
          <span class="type-emphasis">/{{ candidate.command.composerPrefix }}</span>
          <span class="type-caption text-muted-foreground truncate">{{ candidate.command.name }}</span>
        </template>
      </button>
    </div>
  </div>
</template>
