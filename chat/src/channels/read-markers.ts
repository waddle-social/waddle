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
   * Send a XEP-0333 displayed marker for the given message. Targets the
   * message's own id rather than `replyableId` because displayed markers
   * mark *what was seen* — the room's stanza-id is fine, but the wire
   * call wants whatever id the message currently advertises. Failures are
   * swallowed (the marker is best-effort; we don't surface a UX error).
   */
  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    void xmppClient.value
      .sendDisplayed(activeSpaceId.value ?? "", activeChannelId.value, targetId)
      .catch(() => undefined);
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
