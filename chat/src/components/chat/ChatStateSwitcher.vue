<script setup lang="ts">
import { css } from "styled-system/css";
import { toggle } from "styled-system/recipes";
import { useChatStatePreference } from "@/preferences/chat-states";

const { mode, setChatStatePreference } = useChatStatePreference();

const rowClass = css({ display: "flex", alignItems: "center", gap: "3" });
const copyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5", fontSize: "15px" });
const captionClass = css({ textStyle: "caption", color: "fg.muted" });
const switchClass = toggle();

function onToggle() {
  setChatStatePreference(mode.value === "send" ? "suppress" : "send");
}
</script>

<template>
  <div :class="rowClass">
    <span :class="copyClass">
      <span id="chat-states-title">Send typing indicators</span>
      <span id="chat-states-description" :class="captionClass">
        Let people see when you're typing (XEP-0085). When off, no chat-state notifications are sent.
      </span>
    </span>
    <button
      type="button"
      role="switch"
      :class="switchClass"
      :aria-checked="mode === 'send'"
      aria-labelledby="chat-states-title"
      aria-describedby="chat-states-description"
      @click="onToggle"
    />
  </div>
</template>
