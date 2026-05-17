import { computed, ref, type ComputedRef, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { roomKey, setLastSeen } from "@/lib/last-seen-store";
import { latestRemoteMessageIdFor } from "@/lib/timeline-state";

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
  messages: Ref<TimelineMessage[]>;
};

export function useChannelReadMarkers(deps: UseChannelReadMarkersDeps) {
  const { xmppClient, activeSpaceId, activeChannelId, messages } = deps;

  const firstUnseenId = ref<string | null>(null);
  const latestRemoteMessageId: ComputedRef<string | null> = computed(() =>
    latestRemoteMessageIdFor(messages.value),
  );

  /**
   * Send a XEP-0333 displayed marker for the given message and — when
   * the message carries an XEP-0359 room-injected stanza-id — also
   * publish an XEP-0490 MDS item so the user's other devices clear
   * the unread badge for this room. Both calls are best-effort.
   */
  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.id ?? messageId;
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    void client
      .sendDisplayed(spaceId, channelId, targetId)
      .catch(() => undefined);
    // XEP-0490 §3 publish — MUC stanza-id by= room JID. Only fires
    // when the room actually stamped the message (XEP-0359 §3.4
    // makes this conditional on room support). The chat id for
    // MUC is the room bare JID, which is also the `by` JID.
    const stanzaId = target?.stanzaId;
    const stanzaIdBy = target?.stanzaIdBy;
    if (stanzaId && stanzaIdBy) {
      void client
        .publishMdsDisplayed(stanzaIdBy, stanzaId, stanzaIdBy)
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
