<script setup lang="ts">
import { computed } from "vue";
import { css } from "styled-system/css";
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

const detailClass = css({ display: "flex", flexDirection: "column", gap: "2.5", userSelect: "text" });
const dlClass = css({ margin: 0, display: "grid", gridTemplateColumns: "120px minmax(0, 1fr)", gap: "{spacing.1.5} {spacing.4}", fontSize: "14px", "& dt": { color: "fg.muted" }, "& dd": { margin: 0, fontFamily: "mono", color: "fg" } });
const madeClass = css({ display: "flex", alignItems: "center", gap: "1", textStyle: "caption", color: "fg.muted" });
const inlineClass = css({ display: "flex", flexDirection: "column", gap: "1", textStyle: "caption", color: "fg.muted", userSelect: "text" });
const inlineShaClass = css({ display: "flex", flexWrap: "wrap", alignItems: "center", columnGap: "3", rowGap: "0.5", fontFamily: "mono" });
</script>

<template>
  <div v-if="layout === 'detail'" :class="detailClass" :title="tooltip">
    <dl :class="dlClass">
      <dt>Web</dt>
      <dd>{{ webShortSha }}</dd>
      <dt>Server</dt>
      <dd>{{ serverShortSha }}</dd>
    </dl>
    <div :class="madeClass">
      <span>Made proudly in</span>
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴󠁧󠁢󠁳󠁣󠁴󠁿</span>
    </div>
  </div>
  <div v-else :class="inlineClass" :title="tooltip">
    <div :class="inlineShaClass">
      <span>web {{ webShortSha }}</span>
      <span>srv {{ serverShortSha }}</span>
    </div>
    <div :class="madeClass">
      <span>Made proudly in</span>
      <span role="img" aria-label="Germany, Norway, and Scotland">🇩🇪🇳🇴🏴󠁧󠁢󠁳󠁣󠁴󠁿</span>
    </div>
  </div>
</template>
