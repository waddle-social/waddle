<script setup lang="ts">
import { css } from "styled-system/css";
import { tag } from "styled-system/recipes";
import { composerPopover } from "./composer-classes";
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

const blockedClass = css({ display: "flex", alignItems: "center", gap: "2", px: "3", py: "2", textStyle: "caption", color: "danger.text", "& svg": { width: "14px", height: "14px", flex: "none" } });
const slashIcon = css({ width: "16px", height: "16px", flex: "none", color: "action.text" });
const nameClass = css({ fontWeight: 700, flex: "none" });
const builtinTag = css({ ml: "auto", flex: "none" });

function candidateKey(candidate: SlashCandidate): string {
  return candidate.kind === "builtin"
    ? `builtin:${candidate.command.name}`
    : `extension:${candidate.command.serviceJid}:${candidate.command.node}`;
}
</script>

<template>
  <div
    :class="[composerPopover.root, isTopPinned ? composerPopover.below : composerPopover.above]"
  >
    <div v-if="blocked" :class="blockedClass">
      <AlertCircle aria-hidden="true" />
      <span>No command <b>/{{ prefix }}</b>. Press Esc, then Enter to send as text.</span>
    </div>
    <template v-else>
      <button
        v-for="(candidate, i) in candidates"
        :key="candidateKey(candidate)"
        type="button"
        :class="composerPopover.option"
        :data-selected="i === selectedIndex || undefined"
        @mousedown.prevent="emit('pick', candidate)"
      >
        <Slash :class="slashIcon" aria-hidden="true" />
        <template v-if="candidate.kind === 'builtin'">
          <span :class="nameClass">{{ candidate.command.usage }}</span>
          <span :class="composerPopover.hint">{{ candidate.command.description }}</span>
          <span :class="[tag(), builtinTag]">Built-in</span>
        </template>
        <template v-else>
          <span :class="nameClass">/{{ candidate.command.composerPrefix }}</span>
          <span :class="composerPopover.hint">{{ candidate.command.name }}</span>
        </template>
      </button>
    </template>
  </div>
</template>
