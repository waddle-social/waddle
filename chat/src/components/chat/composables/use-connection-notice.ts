import { computed, ref, watch } from "vue";
import { AlertCircle, CheckCircle2, RefreshCw, WifiOff } from "lucide-vue-next";
import { getConnectionNoticeCopy } from "@/lib/connection-notice";
import type { XmppStatusSnapshot } from "@/lib/xmpp-client";

/**
 * Connection banner state for the content pane: tracks the offline /
 * reconnecting / error / briefly-celebrated-reconnected lifecycle from
 * the XMPP status snapshot and derives the banner copy, icon, and tone
 * classes the header chip and banner share.
 */
export function useConnectionNotice(input: {
  status: () => XmppStatusSnapshot;
  queuedMessageCount: () => number;
}) {
  const hasSeenOnline = ref(input.status().state === "online");
  const showReconnectedNotice = ref(false);
  let reconnectedNoticeTimeout: ReturnType<typeof setTimeout> | null = null;

  function clearReconnectedNotice() {
    if (reconnectedNoticeTimeout) {
      clearTimeout(reconnectedNoticeTimeout);
      reconnectedNoticeTimeout = null;
    }
    showReconnectedNotice.value = false;
  }

  watch(() => input.status().state, (state, previousState) => {
    if (state === "online") {
      if (previousState && previousState !== "online" && hasSeenOnline.value) {
        clearReconnectedNotice();
        showReconnectedNotice.value = true;
        reconnectedNoticeTimeout = setTimeout(() => {
          showReconnectedNotice.value = false;
          reconnectedNoticeTimeout = null;
        }, 4200);
      }
      hasSeenOnline.value = true;
      return;
    }

    clearReconnectedNotice();
  });

  const connectionNotice = computed(() =>
    getConnectionNoticeCopy({
      status: input.status(),
      queuedMessageCount: input.queuedMessageCount(),
      showReconnected: showReconnectedNotice.value,
    }),
  );

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

  const connectionStatusClasses = computed(() => {
    switch (connectionNotice.value?.tone) {
      case "offline":
        // Passive state — user already knows they're disconnected.
        // Keep the chip quiet: muted bg tint, no glow halo.
        return {
          banner: "bg-muted/35 text-foreground",
          iconWrap: "border-border/70 bg-background/60 text-muted-foreground/80",
          chip: "border-border/70 bg-muted/25 text-muted-foreground/80",
          body: "text-muted-foreground",
        };
      case "reconnecting":
        // Active reconnection — warning-tinted bg + warning-glow halo
        // reach the eye without becoming an alarm. The icon's spinner
        // carries the "moving" signal.
        return {
          banner: "bg-warning/10 text-foreground",
          iconWrap: "border-warning/15 bg-background/60 text-warning/80",
          chip: "chat-connection-chip-glow--warning border-warning/35 bg-warning/10 text-warning/90",
          body: "text-foreground/75",
        };
      case "error":
        // Wants user attention (session expired, etc.). Destructive-
        // tinted bg + destructive-glow halo distinguish it from the
        // reconnecting tone without escalating to a banner.
        return {
          banner: "bg-destructive/10 text-foreground",
          iconWrap: "border-destructive/15 bg-background/60 text-destructive/80",
          chip: "chat-connection-chip-glow--destructive border-destructive/35 bg-destructive/10 text-destructive/90",
          body: "text-foreground/80",
        };
      case "reconnected":
        // Brief celebration. Primary-tinted bg + --glow-strong halo
        // so the chip reads "we're back" before the banner ages out.
        return {
          banner: "bg-primary/8 text-foreground",
          iconWrap: "border-primary/12 bg-background/60 text-primary/80",
          chip: "chat-connection-chip-glow--primary border-primary/35 bg-primary/10 text-primary/90",
          body: "text-foreground/75",
        };
      default:
        return null;
    }
  });

  return {
    connectionNotice,
    connectionStatusIcon,
    connectionStatusClasses,
    clearReconnectedNotice,
  };
}
