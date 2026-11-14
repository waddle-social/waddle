<script setup lang="ts">
import { css } from "styled-system/css";
import { toggle } from "styled-system/recipes";
import { useReadReceiptPreference } from "@/preferences/read-receipts";

const { mode, setReadReceiptPreference } = useReadReceiptPreference();

const rowClass = css({ display: "flex", alignItems: "center", gap: "3" });
const copyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5", fontSize: "15px" });
const captionClass = css({ textStyle: "caption", color: "fg.muted" });
const switchClass = toggle();

function onToggle() {
  setReadReceiptPreference(mode.value === "send" ? "suppress" : "send");
}
</script>

<template>
  <div :class="rowClass">
    <span :class="copyClass">
      <span id="read-receipts-title">Send read receipts</span>
      <span id="read-receipts-description" :class="captionClass">
        Let people see when you've read their messages (XEP-0333).
      </span>
    </span>
    <button
      type="button"
      role="switch"
      :class="switchClass"
      :aria-checked="mode === 'send'"
      aria-labelledby="read-receipts-title"
      aria-describedby="read-receipts-description"
      @click="onToggle"
    />
  </div>
</template>
