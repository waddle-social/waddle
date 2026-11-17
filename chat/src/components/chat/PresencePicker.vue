<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Check } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { presence } from "styled-system/recipes";
import { $presenceMode, $selfShow, pickPresence } from "@/presence/presence-store";
import type { ManualStatus } from "@/presence/effective-show";

const mode = useStore($presenceMode);
const selfShow = useStore($selfShow);

type DotState = "available" | "away" | "dnd";

const options: { status: ManualStatus; label: string; dot: DotState }[] = [
  { status: "available", label: "Available", dot: "available" },
  // RFC 6121 `<show>chat</show>` — "free for chat". An available substate, so
  // it shares the green dot; contacts render it as Available (ADR-010 fold).
  { status: "chat", label: "Free to chat", dot: "available" },
  { status: "away", label: "Away", dot: "away" },
  { status: "dnd", label: "Do Not Disturb", dot: "dnd" },
];

const isAutomatic = computed(() => mode.value.kind === "automatic");

function isActive(status: ManualStatus): boolean {
  return mode.value.kind === "manual" && mode.value.status === status;
}

const current = computed(() => options.find((o) => o.status === selfShow.value) ?? options[0]!);

// Settings-Menu board: the current status row, then the choices in a
// nested surface.2 box. Dots are the preset `presence` shapes, unpinned.
const staticDot = css({ position: "static", flex: "none" });
const currentDot = computed(() => cx(presence({ state: current.value.dot }), staticDot, css({ "--ring-bg": "{colors.surface}" })));
function optionDot(state: DotState): string {
  return cx(presence({ state }), staticDot, css({ "--ring-bg": "{colors.surface.2}" }));
}
const root = css({ display: "flex", flexDirection: "column", gap: "1" });
const currentRow = css({ display: "flex", alignItems: "center", gap: "2.5", minHeight: "40px", px: "3", font: "500 14px/1.25 {fonts.body}" });
const meta = css({ color: "fg.muted" });
const box = css({ display: "flex", flexDirection: "column", ml: "6", mr: "1", p: "1", borderRadius: "control", bg: "surface.2" });
const option = css({
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  width: "100%",
  minHeight: "36px",
  px: "3",
  border: 0,
  borderRadius: "10px",
  bg: "transparent",
  color: "fg",
  fontSize: "sm",
  textAlign: "left",
  cursor: "pointer",
  _hover: { bg: "state.hover" },
  "&[aria-checked=true]": { bg: "state.selected", fontWeight: 600 },
  "& > svg": { width: "16px", height: "16px", flex: "none", color: "fg.muted" },
  _coarse: { minHeight: "44px" },
});
const label = css({ flex: 1 });
const reset = cx(option, css({ color: "action.text", fontSize: "xs", fontWeight: 600 }));
</script>

<template>
  <div role="radiogroup" aria-label="Set your status" :class="root">
    <div :class="currentRow">
      <span :class="currentDot" aria-hidden="true" />
      <span>{{ current.label }}<span v-if="isAutomatic" :class="meta"> · Automatic</span></span>
    </div>
    <div :class="box">
      <button
        v-for="opt in options"
        :key="opt.status"
        role="radio"
        type="button"
        :aria-checked="isActive(opt.status)"
        :class="option"
        @click="pickPresence(opt.status)"
      >
        <span :class="optionDot(opt.dot)" aria-hidden="true" />
        <span :class="label">{{ opt.label }}</span>
        <Check v-if="isActive(opt.status)" aria-hidden="true" />
      </button>
      <button
        v-if="!isAutomatic"
        type="button"
        :class="reset"
        @click="pickPresence('reset')"
      >
        <span :class="label">Reset to automatic</span>
      </button>
    </div>
  </div>
</template>
