<script setup lang="ts">
import { btn } from "styled-system/recipes";
import { css, cx } from "styled-system/css";
import { connectionNoticeClasses } from "@/components/chat/composables/use-connection-notice";

/**
 * Superseded-session recovery banner for the non-conversation surfaces
 * (dashboard, feed, events, threads, unread, settings). The superseded
 * latch blocks every automatic reconnect, and `ContentArea`'s connection
 * banner only exists on the conversation surface — without this banner a
 * superseded tab parked on any sibling surface has no visible way to
 * invoke recovery short of navigating away or reloading.
 *
 * Same notice card as ContentArea's connection banner (Connection-Notices
 * board, "resumed in another tab" state).
 */
defineProps<{
  detail?: string;
}>();

const emit = defineEmits<{
  recover: [];
}>();

const cls = connectionNoticeClasses;
const cardClass = cx(cls.card, css({ borderColor: "action.text" }));
const actionClass = btn({ variant: "primary", size: "sm" });
</script>

<template>
  <div role="status" aria-live="polite" aria-atomic="true" :class="cardClass">
    <div :class="cls.text">
      <p :class="cls.title">Session resumed in another tab</p>
      <p :class="cls.body">
        {{ (detail ?? "").trim() || "This session was resumed in another tab." }} Reconnect to continue from this tab.
      </p>
    </div>
    <button type="button" :class="actionClass" @click="emit('recover')">Reconnect</button>
  </div>
</template>
