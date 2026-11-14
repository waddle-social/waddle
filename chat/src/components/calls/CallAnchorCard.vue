<script setup lang="ts">
import { computed } from "vue";
import { MessageSquare, Phone, Video } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, liveDot } from "styled-system/recipes";
import type { CallAnchorCardState } from "@/lib/call-thread-anchor";

const props = defineProps<{
  state: CallAnchorCardState;
}>();

const emit = defineEmits<{
  join: [];
  openThread: [threadId: string];
}>();

const isLive = computed(() => props.state.status === "live");
const MediaIcon = computed(() => props.state.media.video ? Video : Phone);
const participantSummary = computed(() => {
  if (props.state.participantLabels.length === 0) return "No one connected";
  const visible = props.state.participantLabels.slice(0, 3).join(", ");
  const extra = props.state.participantLabels.length - 3;
  return extra > 0 ? `${visible} +${extra}` : visible;
});
const messageCountLabel = computed(() =>
  `${props.state.messageCount} ${props.state.messageCount === 1 ? "message" : "messages"} in call chat`
);
const showAction = computed(() => props.state.actionLabel !== null);

function joinCall() {
  if (!isLive.value || props.state.actionDisabled) return;
  emit("join");
}

function openThread() {
  if (!props.state.threadId) return;
  emit("openThread", props.state.threadId);
}

// "Started a huddle" card (Huddle-Join board): live = surface card with an
// aurora icon and Join; ended = dashed, dimmed. Marker classes stay for tests.
const rootClass = css({
  maxWidth: "460px",
  padding: "4",
  borderRadius: "16px",
  bg: "surface",
  border: "1px solid {colors.border.soft}",
  color: "fg",
  "&.call-anchor-card--ended": { padding: "12px 16px", bg: "transparent", border: "1px dashed {colors.border}", opacity: 0.7 },
});
const rowClass = css({ display: "flex", minWidth: 0, alignItems: "center", gap: "2.5" });
const iconClass = css({
  position: "relative",
  display: "inline-flex",
  flex: "none",
  width: "36px",
  height: "36px",
  alignItems: "center",
  justifyContent: "center",
  borderRadius: "pill",
  bg: "live.subtle",
  color: "live.text",
  "& > svg": { width: "18px", height: "18px" },
  ".call-anchor-card--ended &": { width: "auto", height: "auto", bg: "transparent", color: "fg.muted" },
});
const pulseClass = cx("call-anchor-card__pulse", liveDot(), css({ position: "absolute", top: 0, right: 0, width: "10px", height: "10px", boxShadow: "0 0 0 2px {colors.surface}" }));
const copyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", lineHeight: "1.35" });
const titleRowClass = css({ display: "flex", minWidth: 0, alignItems: "center", gap: "2" });
const titleClass = css({ margin: 0, font: "700 15px/1.35 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const countClass = css({ textStyle: "caption", color: "fg.muted", fontVariantNumeric: "tabular-nums" });
const summaryClass = css({ margin: 0, textStyle: "caption", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const joinClass = cx(btn({ variant: "live", size: "sm" }), css({ flex: "none", _coarse: { height: "44px" } }));
const threadClass = css({
  display: "inline-flex",
  maxWidth: "100%",
  alignItems: "center",
  gap: "1.5",
  marginTop: "3",
  padding: 0,
  border: 0,
  bg: "transparent",
  font: "600 13px/1.3 {fonts.body}",
  color: "action.text",
  cursor: "pointer",
  _hover: { textDecoration: "underline" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px", borderRadius: "xs" },
  "& svg": { width: "14px", height: "14px", flex: "none" },
  "& span": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
});
</script>

<template>
  <section
    :class="[rootClass, isLive ? 'call-anchor-card--live' : 'call-anchor-card--ended']"
    :aria-label="state.ariaLabel"
  >
    <div :class="rowClass">
      <div :class="iconClass">
        <component :is="MediaIcon" aria-hidden="true" />
        <span v-if="isLive" :class="pulseClass" aria-hidden="true" />
      </div>
      <div :class="copyClass">
        <div :class="titleRowClass">
          <p :class="titleClass">{{ state.title }}</p>
          <span :class="countClass">{{ state.participantCount }}</span>
        </div>
        <p :class="summaryClass">{{ participantSummary }}</p>
      </div>
      <button
        v-if="showAction"
        type="button"
        :class="joinClass"
        :disabled="state.actionDisabled"
        @click.stop="joinCall"
      >
        <component :is="MediaIcon" aria-hidden="true" />
        <span>{{ state.actionLabel }}</span>
      </button>
    </div>
    <button
      v-if="state.threadId"
      type="button"
      :class="threadClass"
      @click.stop="openThread"
    >
      <MessageSquare aria-hidden="true" />
      <span>{{ messageCountLabel }} ›</span>
    </button>
  </section>
</template>
