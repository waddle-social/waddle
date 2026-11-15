<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
// Admin V2 — single affiliation row used by ChannelDetailDrawer.
// Decoupled from the drawer so the mutation choice (set new
// affiliation) is composed via a v-model-style event and the parent
// owns the network call.
import { css } from "styled-system/css";
import type { WasmAdminChannelAffiliationEntry } from "@/lib/xmpp";
import { adminClasses as a } from "@/components/admin/admin-classes";

type Affiliation = "owner" | "admin" | "member" | "none" | "outcast";
const AFFILIATION_OPTIONS = (["owner", "admin", "member", "none", "outcast"] as const satisfies readonly Affiliation[]).map((value) => ({ value, label: value }));

defineProps<{
  entry: WasmAdminChannelAffiliationEntry;
  mutating?: boolean;
}>();

const emit = defineEmits<{
  change: [affiliation: Affiliation];
}>();

const jidClass = css({ font: "12px {fonts.mono}", color: "fg", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });

</script>

<template>
  <div :class="a.subRow">
    <div :class="a.rowMain">
      <span :class="jidClass">{{ entry.jid }}</span>
      <span v-if="entry.reason" :class="a.sub">{{ entry.reason }}</span>
    </div>
    <AppSelect
      :model-value="entry.affiliation"
      :options="AFFILIATION_OPTIONS"
      :disabled="mutating"
      :class="a.smallSelect"
      :aria-label="`Affiliation for ${entry.jid}`"
      @update:model-value="emit('change', $event)"
    />
  </div>
</template>
