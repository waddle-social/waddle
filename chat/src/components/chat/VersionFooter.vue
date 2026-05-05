<script setup lang="ts">
import { computed } from "vue";
import { extractServerCommitSha, extractServerReleaseVersion, type XmppServerVersion } from "@/shell/version";

const props = defineProps<{
  webCommitSha?: string;
  serverVersion?: XmppServerVersion | null;
  layout?: "detail" | "inline";
}>();

const layout = computed(() => props.layout ?? "detail");

const webShortSha = computed(() => (props.webCommitSha ?? "unknown").slice(0, 7));
const serverShortSha = computed(() => {
  const sha = extractServerCommitSha(props.serverVersion ?? null);
  if (sha) return sha.slice(0, 7);
  if (!props.serverVersion) return "…";
  return extractServerReleaseVersion(props.serverVersion) ?? "unknown";
});
const tooltip = computed(
  () => `web ${props.webCommitSha ?? "unknown"}\nserver ${props.serverVersion?.version ?? "unknown"}`,
);
</script>

<template>
  <div
    v-if="layout === 'detail'"
    class="type-version flex flex-col gap-2 text-muted-foreground/75 select-text"
    :title="tooltip"
  >
    <div class="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-1">
      <span class="type-version-label text-muted-foreground/60">Web</span>
      <span class="type-mono text-foreground/80">{{ webShortSha }}</span>
      <span class="type-version-label text-muted-foreground/60">Server</span>
      <span class="type-mono text-foreground/80">{{ serverShortSha }}</span>
    </div>
    <div class="type-version flex items-center justify-center gap-1 border-t border-border/50 pt-2 text-muted-foreground/70">
      <span>Made proudly in</span>
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴󠁧󠁢󠁳󠁣󠁴󠁿</span>
    </div>
  </div>
  <div
    v-else
    class="type-version flex flex-col gap-1 text-muted-foreground/70 select-text"
    :title="tooltip"
  >
    <div class="type-mono flex flex-wrap items-center gap-x-3 gap-y-0.5">
      <span>web {{ webShortSha }}</span>
      <span>srv {{ serverShortSha }}</span>
    </div>
    <div class="flex items-center gap-1">
      <span>Made proudly in</span>
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴󠁧󠁢󠁳󠁣󠁴󠁿</span>
    </div>
  </div>
</template>
