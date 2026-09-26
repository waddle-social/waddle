<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { AlertCircle, X } from "lucide-vue-next";
import { toast as toastRecipe } from "styled-system/recipes";
import { $callState, $lastCallError, clearLastCallError } from "@/lib/calls/call-store";

// Styled like every other toast (opaque ink); the danger border marks
// it as a failure.
const cls = toastRecipe();

const state = useStore($callState);
const lastError = useStore($lastCallError);

// `IncomingCallToast`, `OutgoingCallToast` (live branch), and
// `CallOverlay` already render `$lastCallError` inline when their
// phase is active. This toast covers the gap: pre-transition
// rejections from `beginMucCall` / `beginOutgoingCall` set the
// error before `$callState` ever leaves `idle`, so without a
// surface that mounts in idle the error is invisible — and clicking
// the channel call button just appears to do nothing. The `ended`
// branch of `OutgoingCallToast` also omits the error row, so cover
// that too.
const visible = computed(() => {
  if (!lastError.value) return false;
  const phase = state.value.phase;
  return phase === "idle" || phase === "ended";
});
</script>

<template>
  <div
    v-if="visible"
    :class="[cls.root, 'call-toast fixed top-6 right-6 z-50 max-w-sm animate-slide-up']"
    style="border-color: var(--destructive)"
    role="alert"
    aria-live="assertive"
    aria-label="Call error"
  >
    <AlertCircle class="mt-0.5 h-4 w-4 shrink-0 text-destructive-text" aria-hidden="true" />
    <div :class="[cls.description, 'min-w-0 flex-1']">{{ lastError }}</div>
    <button
      :class="[cls.closeTrigger, 'inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md']"
      type="button"
      aria-label="Dismiss"
      @click="clearLastCallError"
    >
      <X class="h-3.5 w-3.5" aria-hidden="true" />
    </button>
  </div>
</template>
