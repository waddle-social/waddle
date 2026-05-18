<script setup lang="ts">
// Admin V2 — single affiliation row used by ChannelDetailDrawer.
// Decoupled from the drawer so the mutation choice (set new
// affiliation) is composed via a v-model-style event and the parent
// owns the network call.
import type { WasmAdminChannelAffiliationEntry } from "@/lib/xmpp";

type Affiliation = "owner" | "admin" | "member" | "none" | "outcast";
const AFFILIATIONS: Affiliation[] = ["owner", "admin", "member", "none", "outcast"];

defineProps<{
  entry: WasmAdminChannelAffiliationEntry;
  mutating?: boolean;
}>();

const emit = defineEmits<{
  change: [affiliation: Affiliation];
}>();

function onSelect(event: Event) {
  const target = event.target as HTMLSelectElement;
  const value = target.value as Affiliation;
  emit("change", value);
}
</script>

<template>
  <div class="flex items-center gap-2 rounded-md border border-border bg-card px-2.5 py-2">
    <div class="flex flex-col gap-0.5 min-w-0 flex-1">
      <span class="font-mono type-caption truncate">{{ entry.jid }}</span>
      <span v-if="entry.reason" class="type-caption text-muted-foreground truncate">{{ entry.reason }}</span>
    </div>
    <select
      :value="entry.affiliation"
      :disabled="mutating"
      class="chat-field-control type-caption"
      :aria-label="`Affiliation for ${entry.jid}`"
      @change="onSelect"
    >
      <option v-for="a in AFFILIATIONS" :key="a" :value="a">{{ a }}</option>
    </select>
  </div>
</template>
