<script setup lang="ts">
import { css, cx } from "styled-system/css";
import { useCallElapsed } from "@/lib/calls/use-call-elapsed";
import CallConnectionIndicator from "./CallConnectionIndicator.vue";

/**
 * Status-only stage-header for the in-call surfaces (split + expanded).
 *
 * Shows the call title, a live elapsed-time timer, a contextual subline,
 * the self-connection-quality indicator (relocated here from the control
 * bar), and an inert recording-indicator slot. Carries NO actions — every
 * control lives in `CallControls`. The timer's start instant is owned by
 * the `$callActiveSince` store (via `useCallElapsed`), so it keeps
 * counting across the split↔expanded remount instead of resetting.
 */
const props = withDefaults(
  defineProps<{
    /** The call title — the peer's name (1:1) or the room name (group). */
    title: string;
    /** Contextual status under the title ("Waiting for others…", "Video
     *  call", …). Empty hides the subline row. */
    subline?: string;
    /** Recording-indicator slot. Wired but driven `false` until recording
     *  lands in a later slice — the dot only renders when this is `true`. */
    recording?: boolean;
    /** Tightens padding for the inline split surface. */
    compact?: boolean;
  }>(),
  { subline: "", recording: false, compact: false },
);

const { running, label } = useCallElapsed();

// HuddleStage header: status dot, display title + caption, then the
// timer, connection chip and the (inert) recording slot. The marker
// classes (`call-stage-header__*`) are kept for tests.
const rootClass = css({
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  padding: "12px 16px",
  flexWrap: "wrap",
  color: "fg",
  "& .call-stage-header__recording": {
    display: "inline-flex",
    alignItems: "center",
    gap: "1.5",
    flex: "none",
    color: "danger.text",
    font: "600 13px/1 {fonts.body}",
  },
});
const recordingDotClass = css({ flex: "none", width: "10px", height: "10px", borderRadius: "pill", bg: "currentColor" });
const compactClass = css({ padding: "10px 12px" });
const dotClass = css({
  flex: "none",
  width: "10px",
  height: "10px",
  borderRadius: "pill",
  bg: "fg.soft",
  transition: "background-color {durations.base} {easings.out}",
});
const dotLiveClass = css({
  bg: "presence.available",
  boxShadow: "0 0 0 4px color-mix(in srgb, {colors.presence.available} 20%, transparent)",
  animation: "wd-pulse {durations.ambient} {easings.out} infinite",
  _motionReduce: { animation: "none" },
});
const metaClass = css({ display: "flex", flexDirection: "column", minWidth: 0, flex: "1 1 auto", lineHeight: "1.3" });
const truncate = css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const titleClass = cx(truncate, css({ font: "700 15px/1.3 {fonts.display}", color: "fg" }));
const sublineClass = cx(truncate, css({ textStyle: "caption", color: "fg.muted" }));
const timerClass = css({ flex: "none", font: "600 13px/1 {fonts.body}", fontVariantNumeric: "tabular-nums", color: "fg.muted" });
</script>

<template>
  <header :class="[rootClass, props.compact && compactClass]">
    <!-- The live dot only goes green + pulses once the room is connected; before
         that it stays muted and static so it doesn't imply the call is already
         live. Always present so the connect transition doesn't reflow. -->
    <span
      :class="[dotClass, running && dotLiveClass, running && 'call-stage-header__live-dot--live']"
      aria-hidden="true"
    />
    <div :class="metaClass">
      <div :class="titleClass">{{ props.title }}</div>
      <div v-if="props.subline" :class="sublineClass">
        {{ props.subline }}
      </div>
    </div>
    <!-- The timer role applies only to an actual duration; while connecting this
         is plain status text, so the role and label are dropped until it runs.
         The per-second tick is non-announcing so it never floods a screen reader. -->
    <span
      :class="timerClass"
      :role="running ? 'timer' : undefined"
      :aria-label="running ? 'Call duration' : undefined"
      aria-live="off"
      aria-atomic="true"
    >{{ running ? label : "Connecting…" }}</span>
    <CallConnectionIndicator />
    <!-- Inert capture-state slot: a present-but-empty polite live region so
         that, once capture lands and flips `recording` on, the inserted label
         is announced. Empty (no dot, no label) until then. -->
    <span class="call-stage-header__recording" role="status" aria-live="polite">
      <template v-if="props.recording">
        <span :class="['call-stage-header__recording-dot', recordingDotClass]" aria-hidden="true" />
        <span>Recording</span>
      </template>
    </span>
  </header>
</template>
