import { computed, ref, watch, type Ref } from "vue";
import {
  resumeCallAudioPlayback,
  type CallAudioResumeTarget,
} from "./call-audio-playback";
import { CallAudioResumeAttemptGuard } from "./call-audio-resume-attempt";
import type { CallState } from "./types";

export function useCallAudioPlaybackPromptController(
  blocked: Ref<boolean>,
  callState: Ref<CallState>,
  engine: CallAudioResumeTarget,
) {
  const resumeFailed = ref(false);
  const resuming = ref(false);
  const resumeAttempts = new CallAudioResumeAttemptGuard();
  const visible = computed(() => blocked.value && callState.value.phase === "active");
  const activeCallSid = computed(() =>
    callState.value.phase === "active" ? callState.value.sid : null,
  );

  function resetResumeState(): void {
    resumeAttempts.reset();
    resumeFailed.value = false;
    resuming.value = false;
  }

  watch(
    visible,
    (isVisible) => {
      if (!isVisible) resetResumeState();
    },
    { flush: "sync" },
  );

  watch(activeCallSid, resetResumeState, { flush: "sync" });

  async function enableAudio(): Promise<void> {
    if (resuming.value) return;
    const attempt = resumeAttempts.begin(activeCallSid.value);
    resumeFailed.value = false;
    resuming.value = true;
    try {
      await resumeCallAudioPlayback(engine, () => {
        if (!attempt.matches(activeCallSid.value)) return;
        resumeFailed.value = true;
      });
    } finally {
      if (attempt.matches(activeCallSid.value)) {
        resuming.value = false;
      }
    }
  }

  return {
    activeCallSid,
    enableAudio,
    resumeFailed,
    resuming,
    visible,
  };
}
