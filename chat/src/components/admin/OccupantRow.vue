<script setup lang="ts">
// Admin V2 — single occupant row used by ChannelDetailDrawer. Shows
// nick + real JID + role and affiliation tags and a "Kick" button.
import { UserMinus } from "lucide-vue-next";
import { btn } from "styled-system/recipes";
import { adminClasses as a } from "@/components/admin/admin-classes";
import type { WasmAdminChannelOccupantEntry } from "@/lib/xmpp";

defineProps<{
  entry: WasmAdminChannelOccupantEntry;
  kicking?: boolean;
}>();

const emit = defineEmits<{
  kick: [];
}>();
</script>

<template>
  <div :class="a.subRow">
    <div :class="a.rowMain">
      <b :class="a.truncate">{{ entry.nick }}</b>
      <span :class="a.jid">{{ entry.real_jid }}</span>
    </div>
    <span :class="a.tag">{{ entry.role }}</span>
    <span :class="a.tag">{{ entry.affiliation }}</span>
    <button
      type="button"
      :class="btn({ variant: 'danger', size: 'sm' })"
      :disabled="kicking"
      :aria-label="`Kick ${entry.nick}`"
      @click="emit('kick')"
    >
      <UserMinus aria-hidden="true" />
      <span>{{ kicking ? "Kicking…" : "Kick" }}</span>
    </button>
  </div>
</template>
