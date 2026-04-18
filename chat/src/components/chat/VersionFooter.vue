<script setup lang="ts">
import { computed } from "vue";
import { extractServerSha, extractServerShortVersion, type ServerVersion } from "@/composables/useVersion";

const props = defineProps<{
  webCommitSha?: string;
  serverVersion?: ServerVersion | null;
  layout?: "stacked" | "inline";
}>();

const layout = computed(() => props.layout ?? "stacked");

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
    v-if="layout === 'stacked'"
    class="flex flex-col items-center gap-0.5 text-[9px] leading-tight text-muted-foreground/60 select-text"
    :title="tooltip"
  >
    <span class="font-mono">w {{ webShortSha }}</span>
    <span class="font-mono">s {{ serverShortSha }}</span>
    <span class="text-center leading-tight">
      Made proudly in<br />
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴󠁧󠁢󠁳󠁣󠁴󠁿</span>
    </span>
  </div>
  <div
    v-else
    class="flex items-center justify-center gap-3 px-3 py-2 text-[10px] text-muted-foreground/70 select-text whitespace-nowrap"
    :title="tooltip"
  >
    <span class="font-mono">web {{ webShortSha }}</span>
    <span class="font-mono">srv {{ serverShortSha }}</span>
    <span class="flex items-center gap-1">
      <span>Made proudly in</span>
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴󠁧󠁢󠁳󠁣󠁴󠁿</span>
    </span>
  </div>
</template>
