<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Check } from "lucide-vue-next";
import { $presenceMode, $selfShow, pickPresence } from "@/presence/presence-store";
import type { ManualStatus } from "@/presence/effective-show";

const mode = useStore($presenceMode);
const selfShow = useStore($selfShow);

const options: { status: ManualStatus; label: string; dot: string }[] = [
  { status: "available", label: "Available", dot: "bg-success/75" },
  // RFC 6121 `<show>chat</show>` — "free for chat". An available substate, so
  // it shares the green dot; contacts render it as Available (ADR-010 fold).
  { status: "chat", label: "Free to chat", dot: "bg-success/75" },
  { status: "away", label: "Away", dot: "bg-warning/75" },
  { status: "dnd", label: "Do Not Disturb", dot: "bg-destructive/75" },
];

const isAutomatic = computed(() => mode.value.kind === "automatic");

function isActive(status: ManualStatus): boolean {
  return mode.value.kind === "manual" && mode.value.status === status;
}

const current = computed(() => options.find((o) => o.status === selfShow.value) ?? options[0]!);
</script>

<template>
  <div role="radiogroup" aria-label="Set your status" class="flex flex-col gap-0.5">
    <div class="flex items-center gap-2 px-2.5 py-1">
      <span class="h-2 w-2 flex-shrink-0 rounded-full" :class="current.dot" aria-hidden="true" />
      <span class="type-meta text-muted-foreground">
        {{ current.label }}<template v-if="isAutomatic"> · Automatic</template>
      </span>
    </div>
    <button
      v-for="opt in options"
      :key="opt.status"
      role="radio"
      type="button"
      :aria-checked="isActive(opt.status)"
      class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-foreground transition-colors duration-200 hover:bg-muted"
      @click="pickPresence(opt.status)"
    >
      <span class="h-2 w-2 flex-shrink-0 rounded-full" :class="opt.dot" aria-hidden="true" />
      <span class="flex-1">{{ opt.label }}</span>
      <Check v-if="isActive(opt.status)" class="h-3.5 w-3.5 flex-shrink-0 text-primary/70" />
    </button>
    <button
      v-if="!isAutomatic"
      type="button"
      class="type-menu-item flex h-9 w-full items-center gap-2 rounded-lg px-2.5 text-left text-muted-foreground transition-colors duration-200 hover:bg-muted"
      @click="pickPresence('reset')"
    >
      <span class="flex-1">Reset to automatic</span>
    </button>
  </div>
</template>
