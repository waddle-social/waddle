import { computed, onUnmounted, ref, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callActiveSince,
  callElapsedMs,
  formatCallDuration,
} from "./call-duration";

/**
 * Live elapsed-time clock for the call stage-header.
 *
 * Reads the store-owned `$callActiveSince` stamp (set by the engine on
 * the `connected` event, cleared on `disconnected`) and re-renders a `M:SS` /
 * `H:MM:SS` label once per second. Keeping the start instant in the
 * store — not in component state — means the timer survives the
 * split↔expanded remount of the surface that hosts the header.
 *
 * `running` is `false` until the call connects, so the header can show
 * "Connecting…" rather than a stuck `0:00`. The per-second tick only
 * arms in a browser; under SSR (`window` undefined) the label is
 * computed once from the current clock, which keeps the harness
 * deterministic and leak-free.
 */
export function useCallElapsed(): {
  running: ComputedRef<boolean>;
  label: ComputedRef<string>;
} {
  const since = useStore($callActiveSince);
  const now = ref(Date.now());

  if (typeof window !== "undefined") {
    const timer = setInterval(() => {
      now.value = Date.now();
    }, 1000);
    onUnmounted(() => clearInterval(timer));
  }

  const running = computed(() => since.value !== null);
  const label = computed(() =>
    formatCallDuration(callElapsedMs(since.value, now.value)),
  );

  return { running, label };
}
