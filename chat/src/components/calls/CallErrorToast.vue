<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { AlertCircle } from "lucide-vue-next";
import { $callState, $lastCallError, clearLastCallError } from "@/lib/calls/call-store";

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
    class="fixed top-6 right-6 z-50 max-w-sm rounded-xl border border-destructive/30 bg-destructive/10 p-3 shadow-2xl glass-surface"
    role="alert"
    aria-live="assertive"
    aria-label="Call error"
  >
    <div class="flex items-start gap-2">
      <AlertCircle class="w-4 h-4 mt-0.5 text-destructive flex-shrink-0" />
      <div class="min-w-0 flex-1 type-caption text-destructive">{{ lastError }}</div>
      <button
        class="chat-icon-button chat-icon-button--sm text-destructive/70 hover:bg-destructive/10 hover:text-destructive"
        type="button"
        aria-label="Dismiss"
        @click="clearLastCallError"
      >
        <span aria-hidden="true">×</span>
      </button>
    </div>
  </div>
</template>
