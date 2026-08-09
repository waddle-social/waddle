import type { XmppStatusSnapshot } from "@/lib/xmpp/types";

type ConnectionNoticeTone = "offline" | "reconnecting" | "error" | "reconnected";

export interface ConnectionNoticeCopy {
  tone: ConnectionNoticeTone;
  shortLabel: string;
  title: string;
  body: string;
  actionLabel?: string;
  actionKind?: "recover-superseded";
}

interface ConnectionNoticeInput {
  status: XmppStatusSnapshot;
  queuedMessageCount: number;
  showReconnected: boolean;
}

function queuedMessagesLabel(count: number) {
  return `${count} queued message${count === 1 ? "" : "s"}`;
}

function queuedRetryCopy(count: number) {
  if (count > 0) {
    return `${queuedMessagesLabel(count)} will send once you're connected again.`;
  }
  return "Any queued messages will send once you're connected again.";
}

function queuedHoldCopy(count: number) {
  if (count > 0) {
    return `${queuedMessagesLabel(count)} will stay here and send once you're connected again.`;
  }
  return "Any queued messages will stay here and send once you're connected again.";
}

function queuedResumeCopy(count: number) {
  if (count > 0) {
    return `${queuedMessagesLabel(count)} ${count === 1 ? "is" : "are"} sending now.`;
  }
  return "Any queued messages are sending now.";
}

export function getConnectionNoticeCopy({
  status,
  queuedMessageCount,
  showReconnected,
}: ConnectionNoticeInput): ConnectionNoticeCopy | null {
  if (status.state === "online") {
    if (!showReconnected) return null;
    return {
      tone: "reconnected",
      shortLabel: "back online",
      title: "Back online",
      body: queuedResumeCopy(queuedMessageCount),
    };
  }

  if (status.state === "offline") {
    if (status.kind === "superseded") {
      return {
        tone: "offline",
        shortLabel: "reconnect",
        title: "Session resumed in another tab",
        body: `${status.detail.trim() || "This session was resumed in another tab."} Reconnect to continue from this tab.`,
        actionLabel: "Reconnect",
        actionKind: "recover-superseded",
      };
    }
    return {
      tone: "offline",
      shortLabel: "offline",
      title: "Disconnected",
      body: `You're offline. ${queuedRetryCopy(queuedMessageCount)}`,
    };
  }

  if (status.state === "reconnecting") {
    return {
      tone: "reconnecting",
      shortLabel: "reconnecting",
      title: "Reconnecting…",
      body: `Trying again now. ${queuedRetryCopy(queuedMessageCount)}`,
    };
  }

  const detail = status.detail.trim();
  const help = /session expired|log in again|session needs attention/i.test(detail)
    ? "Your session needs attention. Sign in again to restore live messaging."
    : detail.length > 0
      ? detail
      : "We couldn't restore the connection yet.";

  return {
    tone: "error",
    shortLabel: "needs attention",
    title: "Connection needs attention",
    body: `${help} ${queuedHoldCopy(queuedMessageCount)}`,
  };
}
