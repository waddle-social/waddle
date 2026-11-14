import { computed, onScopeDispose, ref, watch } from "vue";
import { AlertCircle, CheckCircle2, RefreshCw, WifiOff } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { getConnectionNoticeCopy, type ConnectionNoticeCopy } from "@/lib/connection-notice";
import type { XmppStatusSnapshot } from "@/lib/xmpp-client";

/**
 * Connection banner state for the content pane: tracks the offline /
 * reconnecting / error / briefly-celebrated-reconnected lifecycle from
 * the XMPP status snapshot and derives the banner copy, icon, and tone
 * classes the header chip and banner share.
 */
// A reconnect that settles within this window (e.g. a tab refocus that
// finds a dead socket) shows no banner at all: the banner sits in the
// layout above the timeline, so a flash of it shifts the whole pane.
const RECONNECTING_NOTICE_DELAY_MS = 3000;

/**
 * The notice card (Connection-Notices board), shared by ContentArea's
 * banner and SupersededRecoveryBanner. `TONE_ACCENT[tone].banner` adds the
 * state's border colour.
 */
export const connectionNoticeClasses = {
  card: css({
    display: "flex",
    alignItems: "center",
    gap: "14px",
    mx: "gutter",
    my: "3",
    py: "3",
    px: "4",
    borderRadius: "control",
    bg: "surface",
    color: "fg",
    border: "1px solid {colors.border}",
    boxShadow: "overlay",
    animation: "wd-rise {durations.base} {easings.out} backwards",
    _narrow: { flexWrap: "wrap" },
  }),
  text: css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5" }),
  title: css({ margin: 0, font: "700 14px/1.3 {fonts.body}" }),
  body: css({ margin: 0, font: "400 13px/1.45 {fonts.body}", color: "fg.muted" }),
};

// Literal objects per tone: Panda extracts `css()` statically.
const TONE_ACCENT: Record<ConnectionNoticeCopy["tone"], { banner: string; chip: string }> = {
  offline: {
    banner: css({ borderColor: "border.strong" }),
    chip: css({ display: "inline-flex", alignItems: "center", gap: "1.5", height: "28px", px: "2.5", borderRadius: "pill", border: "1px solid", bg: "surface", font: "600 13px/1 {fonts.body}", "& svg": { width: "14px", height: "14px" }, borderColor: "border.strong", color: "fg.muted" }),
  },
  reconnecting: {
    banner: css({ borderColor: "action.text" }),
    chip: css({ display: "inline-flex", alignItems: "center", gap: "1.5", height: "28px", px: "2.5", borderRadius: "pill", border: "1px solid", bg: "surface", font: "600 13px/1 {fonts.body}", "& svg": { width: "14px", height: "14px" }, borderColor: "action.text", color: "action.text" }),
  },
  error: {
    banner: css({ borderColor: "danger.text" }),
    chip: css({ display: "inline-flex", alignItems: "center", gap: "1.5", height: "28px", px: "2.5", borderRadius: "pill", border: "1px solid", bg: "surface", font: "600 13px/1 {fonts.body}", "& svg": { width: "14px", height: "14px" }, borderColor: "danger.text", color: "danger.text" }),
  },
  reconnected: {
    banner: css({ borderColor: "success.text" }),
    chip: css({ display: "inline-flex", alignItems: "center", gap: "1.5", height: "28px", px: "2.5", borderRadius: "pill", border: "1px solid", bg: "surface", font: "600 13px/1 {fonts.body}", "& svg": { width: "14px", height: "14px" }, borderColor: "success.text", color: "success.text" }),
  },
};

export function useConnectionNotice(input: {
  status: () => XmppStatusSnapshot;
  queuedMessageCount: () => number;
  reconnectingNoticeDelayMs?: number;
}) {
  const hasSeenOnline = ref(input.status().state === "online");
  const showReconnectedNotice = ref(false);
  const outageNoticeShown = ref(input.status().state !== "online");
  let reconnectedNoticeTimeout: ReturnType<typeof setTimeout> | null = null;
  let reconnectingNoticeTimeout: ReturnType<typeof setTimeout> | null = null;

  function clearReconnectingTimer() {
    if (reconnectingNoticeTimeout) {
      clearTimeout(reconnectingNoticeTimeout);
      reconnectingNoticeTimeout = null;
    }
  }

  function clearReconnectedNotice() {
    if (reconnectedNoticeTimeout) {
      clearTimeout(reconnectedNoticeTimeout);
      reconnectedNoticeTimeout = null;
    }
    showReconnectedNotice.value = false;
  }

  watch(() => input.status().state, (state, previousState) => {
    if (state === "online") {
      clearReconnectingTimer();
      // Celebrate only an outage the user actually saw.
      if (previousState && previousState !== "online" && hasSeenOnline.value && outageNoticeShown.value) {
        clearReconnectedNotice();
        showReconnectedNotice.value = true;
        reconnectedNoticeTimeout = setTimeout(() => {
          showReconnectedNotice.value = false;
          reconnectedNoticeTimeout = null;
        }, 4200);
      }
      outageNoticeShown.value = false;
      hasSeenOnline.value = true;
      return;
    }

    clearReconnectedNotice();
    if (state !== "reconnecting") {
      clearReconnectingTimer();
      outageNoticeShown.value = true;
    } else if (!outageNoticeShown.value && !reconnectingNoticeTimeout) {
      reconnectingNoticeTimeout = setTimeout(() => {
        reconnectingNoticeTimeout = null;
        outageNoticeShown.value = true;
      }, input.reconnectingNoticeDelayMs ?? RECONNECTING_NOTICE_DELAY_MS);
    }
  });

  onScopeDispose(() => {
    clearReconnectingTimer();
    clearReconnectedNotice();
  });

  const connectionNotice = computed(() => {
    const status = input.status();
    if (status.state === "reconnecting" && !outageNoticeShown.value) return null;
    return getConnectionNoticeCopy({
      status,
      queuedMessageCount: input.queuedMessageCount(),
      showReconnected: showReconnectedNotice.value,
    });
  });

  const connectionStatusIcon = computed(() => {
    switch (connectionNotice.value?.tone) {
      case "offline":
        return WifiOff;
      case "reconnecting":
        return RefreshCw;
      case "error":
        return AlertCircle;
      case "reconnected":
        return CheckCircle2;
      default:
        return WifiOff;
    }
  });

  // Polar Huddle connection notice (Connection-Notices board): one
  // surface card whose border carries the state, plus a header chip in the
  // same accent. `connection-chip--<tone>` is a stable hook for tests.
  const connectionStatusClasses = computed(() => {
    const tone = connectionNotice.value?.tone;
    if (!tone) return null;
    const accent = connectionNotice.value?.actionKind === "recover-superseded" ? TONE_ACCENT.reconnecting : TONE_ACCENT[tone];
    return { ...accent, chip: cx(accent.chip, `connection-chip--${tone}`) };
  });

  return {
    connectionNotice,
    connectionStatusIcon,
    connectionStatusClasses,
    clearReconnectedNotice,
  };
}
