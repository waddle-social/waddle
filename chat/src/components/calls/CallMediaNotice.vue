<script setup lang="ts">
import { computed, reactive } from "vue";
import { useStore } from "@nanostores/vue";
import { MicOff, VideoOff, X } from "lucide-vue-next";
import {
  $callMediaIssues,
  clearMediaIssue,
  mediaIssueMessage,
  type MediaIssueReason,
  type MediaKind,
} from "@/lib/calls/call-media-issues";
import { toggleCam, toggleMic } from "@/lib/calls/call-controls";

/**
 * Non-blocking, info-toned notice shown inside the call surfaces when
 * the local user joined without a working microphone or camera (no
 * device, or permission denied). Unlike the destructive error row it
 * sits next to, this is a recoverable status: it stays for the call
 * lifetime and offers a retry that re-attempts capture (e.g. after the
 * user plugs in a device or grants access in the browser). A deliberate
 * listener can dismiss it for the current call.
 *
 * Renders nothing when there are no media issues.
 */
const issues = useStore($callMediaIssues);

// Per-kind in-flight flag so a tapped retry shows visible "Checking…"
// feedback — without it a retry that fails again (e.g. a denied
// permission the browser won't re-prompt) re-records the SAME reason,
// the message text is byte-identical, and the button looks inert.
const retrying = reactive<Record<MediaKind, boolean>>({ mic: false, cam: false });

type NoticeRow = {
  kind: MediaKind;
  reason: MediaIssueReason;
  message: string;
  actionLabel: string;
};

const rows = computed<NoticeRow[]>(() => {
  const out: NoticeRow[] = [];
  const { mic, cam } = issues.value;
  if (mic) {
    out.push({ kind: "mic", reason: mic, message: mediaIssueMessage("mic", mic), actionLabel: "Enable mic" });
  }
  if (cam) {
    out.push({ kind: "cam", reason: cam, message: mediaIssueMessage("cam", cam), actionLabel: "Turn on camera" });
  }
  return out;
});

async function retry(kind: MediaKind): Promise<void> {
  if (retrying[kind]) return;
  retrying[kind] = true;
  try {
    // toggleMic/toggleCam double as the capture-retry path: from the
    // muted state they attempt to publish again, clearing the issue on
    // success or re-recording it on failure.
    await (kind === "mic" ? toggleMic() : toggleCam());
  } finally {
    retrying[kind] = false;
  }
}

function dismiss(kind: MediaKind): void {
  clearMediaIssue(kind);
}
</script>

<template>
  <div v-if="rows.length" class="call-media-notice" role="status" aria-live="polite">
    <div v-for="row in rows" :key="row.kind" class="call-media-notice__row">
      <component
        :is="row.kind === 'mic' ? MicOff : VideoOff"
        class="call-media-notice__icon"
        aria-hidden="true"
      />
      <span class="call-media-notice__text">{{ row.message }}</span>
      <button
        type="button"
        class="call-media-notice__action"
        :disabled="retrying[row.kind]"
        @click="retry(row.kind)"
      >
        {{ retrying[row.kind] ? "Checking…" : row.actionLabel }}
      </button>
      <button
        type="button"
        class="call-media-notice__dismiss"
        :aria-label="`Dismiss ${row.kind === 'mic' ? 'microphone' : 'camera'} notice`"
        @click="dismiss(row.kind)"
      >
        <X class="h-3.5 w-3.5" aria-hidden="true" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.call-media-notice {
  display: flex;
  flex-direction: column;
  gap: var(--space-2xs, 0.25rem);
  /* Info/warning tone — a recoverable limitation, not a hard error, so
   * deliberately NOT the destructive red used by the error row. */
  border-bottom: 1px solid color-mix(in oklab, var(--primary) 28%, transparent);
  background: color-mix(in oklab, var(--primary) 8%, transparent);
  padding: 0.5rem 0.75rem;
}

.call-media-notice__row {
  display: flex;
  align-items: center;
  gap: var(--space-xs, 0.5rem);
  min-width: 0;
}

.call-media-notice__icon {
  width: 1rem;
  height: 1rem;
  flex-shrink: 0;
  color: var(--muted-foreground);
}

.call-media-notice__text {
  flex: 1 1 auto;
  min-width: 0;
  font-size: 0.8125rem;
  line-height: 1.3;
  color: var(--foreground);
}

.call-media-notice__action {
  flex-shrink: 0;
  border-radius: var(--radius-sm);
  padding: 0.25rem 0.5rem;
  font-size: 0.75rem;
  font-weight: 600;
  /* `--foreground` (not `--primary`) on the tinted fill to clear WCAG AA
   * contrast in light mode; the tint + weight + hover still read as a
   * button. */
  color: var(--foreground);
  background: color-mix(in oklab, var(--primary) 16%, transparent);
  transition: background-color 0.18s ease;
}

.call-media-notice__action:hover:not(:disabled) {
  background: color-mix(in oklab, var(--primary) 26%, transparent);
}

.call-media-notice__action:disabled {
  opacity: 0.6;
  cursor: default;
}

.call-media-notice__action:focus-visible,
.call-media-notice__dismiss:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}

.call-media-notice__dismiss {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.5rem;
  height: 1.5rem;
  border-radius: var(--radius-sm);
  color: var(--muted-foreground);
  transition: background-color 0.18s ease, color 0.18s ease;
}

.call-media-notice__dismiss:hover {
  background: color-mix(in oklab, var(--foreground) 8%, transparent);
  color: var(--foreground);
}
</style>
