<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { AlertCircle, X } from "lucide-vue-next";
import { callToastClasses } from "./call-toast-classes";
import { $callState, $lastCallError, clearLastCallError } from "@/lib/calls/call-store";

// Ink in both themes; the danger border marks it as a failure.
const cls = callToastClasses;

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
    :class="cls.error"
    data-theme="night"
    role="alert"
    aria-live="assertive"
    aria-label="Call error"
  >
    <AlertCircle :class="cls.errorIcon" aria-hidden="true" />
    <div :class="cls.copy">
      <b :class="cls.errorTitle">Call error</b>
      <span :class="cls.status">{{ lastError }}</span>
    </div>
    <button :class="cls.dismiss" type="button" aria-label="Dismiss" @click="clearLastCallError">
      <X aria-hidden="true" />
    </button>
  </div>
</template>
