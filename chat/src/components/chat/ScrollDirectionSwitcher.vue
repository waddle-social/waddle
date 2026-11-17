<script setup lang="ts">
import { css, cx } from "styled-system/css";
import { seg } from "styled-system/recipes";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import type { ScrollDirectionMode } from "@/lib/scroll-direction";

const { mode, setScrollDirection } = useScrollDirectionPreference();

const options: ReadonlyArray<{ value: ScrollDirectionMode; label: string }> = [
  { value: "chat", label: "At the bottom" },
  { value: "social", label: "At the top" },
];

const rowClass = css({ display: "flex", alignItems: "center", gap: "3", flexWrap: "wrap" });
const labelClass = css({ flex: 1, minWidth: 0, fontSize: "15px" });
const segClass = cx(seg(), css({ flex: "none" }));
</script>

<template>
  <div :class="rowClass">
    <span id="scroll-direction-label" :class="labelClass">Newest messages</span>
    <div role="radiogroup" aria-labelledby="scroll-direction-label" :class="segClass">
      <button
        v-for="option in options"
        :key="option.value"
        type="button"
        role="radio"
        :aria-checked="mode === option.value"
        @click="setScrollDirection(option.value)"
      >
        {{ option.label }}
      </button>
    </div>
  </div>
</template>
