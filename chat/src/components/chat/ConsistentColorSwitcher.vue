<script setup lang="ts">
import { css } from "styled-system/css";
import { toggle } from "styled-system/recipes";
import { useConsistentColorPreference } from "@/preferences/consistent-color";

const { mode, setConsistentColorPreference } = useConsistentColorPreference();

const rowClass = css({ display: "flex", alignItems: "center", gap: "3" });
const copyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5", fontSize: "15px" });
const captionClass = css({ textStyle: "caption", color: "fg.muted" });
const switchClass = toggle();

function onToggle() {
  setConsistentColorPreference(mode.value === "on" ? "off" : "on");
}
</script>

<template>
  <div :class="rowClass">
    <span :class="copyClass">
      <span id="consistent-color-title">Colorize names and avatars</span>
      <span id="consistent-color-description" :class="captionClass">
        Give each person a consistent color (XEP-0392). Off uses one static color.
      </span>
    </span>
    <button
      type="button"
      role="switch"
      :class="switchClass"
      :aria-checked="mode === 'on'"
      aria-labelledby="consistent-color-title"
      aria-describedby="consistent-color-description"
      @click="onToggle"
    />
  </div>
</template>
