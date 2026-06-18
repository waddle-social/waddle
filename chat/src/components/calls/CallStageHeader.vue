<script setup lang="ts">
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
</script>

<template>
  <header
    class="call-stage-header"
    :class="{ 'call-stage-header--compact': props.compact }"
  >
    <!-- The live dot only goes green + pulses once the room is connected; before
         that it stays muted and static so it doesn't imply the call is already
         live. Always present so the connect transition doesn't reflow. -->
    <span
      class="call-stage-header__live-dot"
      :class="{ 'call-stage-header__live-dot--live': running }"
      aria-hidden="true"
    />
    <div class="call-stage-header__meta">
      <div class="call-stage-header__title type-control truncate">{{ props.title }}</div>
      <div v-if="props.subline" class="call-stage-header__subline type-caption truncate">
        {{ props.subline }}
      </div>
    </div>
    <!-- The timer role applies only to an actual duration; while connecting this
         is plain status text, so the role and label are dropped until it runs.
         The per-second tick is non-announcing so it never floods a screen reader. -->
    <span
      class="call-stage-header__timer type-control"
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
        <span class="call-stage-header__recording-dot" aria-hidden="true" />
        <span class="type-control">Recording</span>
      </template>
    </span>
  </header>
</template>

<style scoped>
.call-stage-header {
  display: flex;
  align-items: center;
  gap: var(--space-sm);
  padding: 0.625rem 1rem;
  border-bottom: 1px solid var(--border);
}

.call-stage-header--compact {
  padding: 0.5rem 0.75rem;
}

.call-stage-header__live-dot {
  flex: none;
  width: 0.5rem;
  height: 0.5rem;
  border-radius: 9999px;
  /* Muted + static while connecting — not yet "live". */
  background: var(--muted-foreground);
}

/* Connected: green + pulsing. */
.call-stage-header__live-dot--live {
  background: oklch(0.7 0.18 145);
  box-shadow: 0 0 0 4px color-mix(in oklab, oklch(0.7 0.18 145) 25%, transparent);
  animation: call-stage-live-pulse 1.6s ease-in-out infinite;
}

@media (prefers-reduced-motion: reduce) {
  .call-stage-header__live-dot--live {
    animation: none;
  }
}

@keyframes call-stage-live-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.55; }
}

.call-stage-header__meta {
  min-width: 0;
  flex: 1 1 auto;
}

.call-stage-header__title {
  color: var(--foreground);
}

.call-stage-header__subline {
  color: var(--muted-foreground);
}

.call-stage-header__timer {
  flex: none;
  color: var(--muted-foreground);
  font-variant-numeric: tabular-nums;
}

.call-stage-header__recording {
  display: inline-flex;
  align-items: center;
  gap: 0.375rem;
  flex: none;
  color: var(--destructive);
}

/* A CSS-native filled circle — unambiguously solid, no SVG paint inheritance. */
.call-stage-header__recording-dot {
  flex: none;
  width: 0.625rem;
  height: 0.625rem;
  border-radius: 9999px;
  background: currentColor;
}
</style>
