import { computed, ref, type ComputedRef, type Ref } from "vue";
import { barePeerJid, type BrowserXmppClient } from "@/lib/xmpp-client";
import type { ChannelSummary } from "@/lib/chat-types";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { roomKey, setLastSeen } from "@/lib/last-seen-store";
import { latestRemoteMessageIdFor } from "@/lib/timeline-state";
import { useReadReceiptPreference } from "@/preferences/read-receipts";
import { reportDisplayedMarkerFailure } from "@/lib/telemetry";

const NS_STANZA_ID = "urn:xmpp:sid:0";

// XEP-0333 chat-marker outbound state for the channel side. Owns:
//   - firstUnseenId: the "new messages" divider anchor, populated by
//     useChannelMamPaging on initial load and cleared on session/channel
//     change.
//   - latestRemoteMessageId: a computed walk-back over the timeline to
//     find the most recent non-self, non-retracted message id (the read
//     anchor that XEP-0333 markers should target).
//   - markDisplayed: outbound `<displayed/>` chat marker, fire-and-forget.
//   - persistLastSeen: helper that records the channel's last-seen id in
//     the local store (consumed by useChannelMamPaging on next load to
//     compute firstUnseenId).

type UseChannelReadMarkersDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activeSpaceId: Ref<string | null>;
  activeChannelId: Ref<string | null>;
  currentChannel: Ref<ChannelSummary | null>;
  messages: Ref<TimelineMessage[]>;
};
type MarkDisplayedOptions = {
  syncMds?: boolean;
};

export function useChannelReadMarkers(deps: UseChannelReadMarkersDeps) {
  const { xmppClient, activeSpaceId, activeChannelId, currentChannel, messages } = deps;

  const firstUnseenId = ref<string | null>(null);
  const { sendDisplayedMarkers } = useReadReceiptPreference();
  const latestRemoteMessageId: ComputedRef<string | null> = computed(() =>
    latestRemoteMessageIdFor(messages.value),
  );

  /**
   * Send a XEP-0333 displayed marker for the given message and — when
   * the message carries an XEP-0359 room-injected stanza-id — also
   * publish an XEP-0490 MDS item so the user's other devices clear
   * the unread badge for this room. Both calls are best-effort.
   */
  function markDisplayed(messageId: string, options: MarkDisplayedOptions = {}) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const target = findMessageById(messages.value, messageId);
    const stanzaId = target?.stanzaId;
    const stanzaIdBy = target?.stanzaIdBy;
    const roomJid = currentChannel.value?.jid;
    const hasStanzaIdSupport = currentChannel.value?.features?.includes(NS_STANZA_ID) === true;
    if (
      !stanzaId ||
      !stanzaIdBy ||
      !roomJid ||
      !hasStanzaIdSupport ||
      barePeerJid(stanzaIdBy) !== barePeerJid(roomJid)
    ) {
      return;
    }
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    // XEP-0201 §3 + XEP-0333: a displayed marker for a threaded message
    // SHOULD echo that message's <thread/> so the marker scopes to the
    // right thread instead of being treated as a channel-wide read.
    const thread = target?.threadId
      ? target.parentThreadId
        ? { id: target.threadId, parent: target.parentThreadId }
        : { id: target.threadId }
      : undefined;
    if (sendDisplayedMarkers.value) {
      void client
        .sendDisplayed(spaceId, channelId, stanzaId, thread)
        .catch(() => reportDisplayedMarkerFailure({
          direction: "send",
          kind: "room",
          reason: "send-failed",
          roundTripMs: elapsedSince(target.createdAt),
        }));
    }
    // XEP-0490 §3 publish — MUC stanza-id by= room JID. Only fires
    // when the room actually stamped the message (XEP-0359 §3.4
    // makes this conditional on room support). The chat id for
    // MUC is the room bare JID, which is also the `by` JID.
    if (options.syncMds !== false) {
      void client
        .publishMdsDisplayed(barePeerJid(stanzaIdBy), stanzaId, barePeerJid(stanzaIdBy))
        .catch(() => undefined);
    }
  }

  function persistLastSeen(channelId: string, messageId: string) {
    setLastSeen(roomKey(channelId), messageId);
  }

  return {
    firstUnseenId,
    latestRemoteMessageId,
    markDisplayed,
    persistLastSeen,
  };
}

function elapsedSince(createdAt: string): number | null {
  const startedAt = Date.parse(createdAt);
  return Number.isFinite(startedAt) ? Math.max(0, Date.now() - startedAt) : null;
}
