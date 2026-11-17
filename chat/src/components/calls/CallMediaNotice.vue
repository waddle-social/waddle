<script setup lang="ts">
import { computed, reactive } from "vue";
import { useStore } from "@nanostores/vue";
import { css } from "styled-system/css";
import { btn, iconbtn } from "styled-system/recipes";
import { MonitorUp, MicOff, VideoOff, X } from "lucide-vue-next";
import {
  $callMediaIssues,
  clearMediaIssue,
  mediaIssueMessage,
  type MediaIssueReason,
  type MediaKind,
} from "@/lib/calls/call-media-issues";
import { toggleCam, toggleMic, toggleScreenShare } from "@/lib/calls/call-controls";

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
const retrying = reactive<Record<MediaKind, boolean>>({
  mic: false,
  cam: false,
  screen: false,
});

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
  if (issues.value.screen) {
    out.push({
      kind: "screen",
      reason: issues.value.screen,
      message: mediaIssueMessage("screen", issues.value.screen),
      actionLabel: "Share screen",
    });
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
    if (kind === "mic") await toggleMic();
    else if (kind === "cam") await toggleCam();
    else await toggleScreenShare();
  } finally {
    retrying[kind] = false;
  }
}

function dismiss(kind: MediaKind): void {
  clearMediaIssue(kind);
}

// HuddleStage notice strip: warning tint, recoverable (not the danger row).
const rootClass = css({ display: "flex", flexDirection: "column", gap: "2", margin: "0 {spacing.4} {spacing.2.5}", _narrow: { mx: "3" } });
const rowClass = css({
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  flexWrap: "wrap",
  padding: "{spacing.2.5} {spacing.3}",
  borderRadius: "12px",
  bg: "color-mix(in srgb, {colors.presence.away} 12%, transparent)",
  border: "1px solid color-mix(in srgb, {colors.presence.away} 35%, transparent)",
  color: "fg",
});
const iconClass = css({ width: "20px", height: "20px", flex: "none", color: "warning.text" });
const textClass = css({ flex: "1 1 240px", minWidth: 0, fontSize: "13px", lineHeight: "1.4" });
const actionClass = btn({ size: "sm" });
const dismissClass = iconbtn({ size: "sm" });
</script>

<template>
  <div v-if="rows.length" :class="rootClass" role="status" aria-live="polite">
    <div v-for="row in rows" :key="row.kind" :class="rowClass">
      <component
        :is="row.kind === 'mic' ? MicOff : row.kind === 'cam' ? VideoOff : MonitorUp"
        :class="iconClass"
        aria-hidden="true"
      />
      <span :class="textClass">{{ row.message }}</span>
      <button
        type="button"
        :class="actionClass"
        :disabled="retrying[row.kind]"
        @click="retry(row.kind)"
      >
        {{ retrying[row.kind] ? "Checking…" : row.actionLabel }}
      </button>
      <button
        type="button"
        :class="dismissClass"
        :aria-label="`Dismiss ${row.kind === 'mic' ? 'microphone' : row.kind === 'cam' ? 'camera' : 'screen sharing'} notice`"
        @click="dismiss(row.kind)"
      >
        <X aria-hidden="true" />
      </button>
    </div>
  </div>
</template>
