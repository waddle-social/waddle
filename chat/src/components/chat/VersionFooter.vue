<script setup lang="ts">
import { computed } from "vue";
import { extractServerSha, extractServerShortVersion, type ServerVersion } from "@/composables/useVersion";

const props = defineProps<{
  webCommitSha?: string;
  serverVersion?: ServerVersion | null;
  layout?: "detail" | "inline";
}>();

const layout = computed(() => props.layout ?? "detail");

const webShortSha = computed(() => (props.webCommitSha ?? "unknown").slice(0, 7));
const serverShortSha = computed(() => {
  const sha = extractServerSha(props.serverVersion ?? null);
  if (sha) return sha.slice(0, 7);
  if (!props.serverVersion) return "…";
  return extractServerShortVersion(props.serverVersion) ?? "unknown";
});
const tooltip = computed(
  () => `web ${props.webCommitSha ?? "unknown"}\nserver ${props.serverVersion?.version ?? "unknown"}`,
);
</script>

<template>
  <div
    v-if="layout === 'detail'"
    class="flex flex-col gap-2 text-[11px] leading-tight text-muted-foreground/75 select-text"
    :title="tooltip"
  >
    <div class="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-1">
      <span class="text-[10px] uppercase tracking-[0.12em] text-muted-foreground/60">Web</span>
      <span class="font-mono text-foreground/80">{{ webShortSha }}</span>
      <span class="text-[10px] uppercase tracking-[0.12em] text-muted-foreground/60">Server</span>
      <span class="font-mono text-foreground/80">{{ serverShortSha }}</span>
    </div>
    <div class="flex items-center justify-center gap-1 border-t border-border/50 pt-2 text-[10px] text-muted-foreground/70">
      <span>Made proudly in</span>
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴</span>
    </div>
  </div>
  <div
    v-else
    class="flex flex-col gap-1 text-[10px] leading-tight text-muted-foreground/70 select-text"
    :title="tooltip"
  >
    <div class="flex flex-wrap items-center gap-x-3 gap-y-0.5 font-mono">
      <span>web {{ webShortSha }}</span>
      <span>srv {{ serverShortSha }}</span>
    </div>
    <div class="flex items-center gap-1">
      <span>Made proudly in</span>
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴</span>
    </div>
  </div>
</template>
