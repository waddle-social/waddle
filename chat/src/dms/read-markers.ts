import { computed, ref, type ComputedRef, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { dmKey, setLastSeen } from "@/lib/last-seen-store";
import { latestRemoteMessageIdFor } from "@/lib/timeline-state";

// XEP-0333 chat-marker outbound state for the DM side. Mirrors
// useChannelReadMarkers; uses `dmKey(barePeerJid(peerJid))` as the
// last-seen-store key instead of the channel's roomKey.

type UseDmReadMarkersDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
};

export function useDmReadMarkers(deps: UseDmReadMarkersDeps) {
  const { xmppClient, activePeerJid, messages } = deps;

  const firstUnseenId = ref<string | null>(null);
  const latestRemoteMessageId: ComputedRef<string | null> = computed(() =>
    latestRemoteMessageIdFor(messages.value),
  );

  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activePeerJid.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    void xmppClient.value
      .sendDmDisplayed(activePeerJid.value, targetId)
      .catch(() => undefined);
  }

  function persistLastSeen(peerJid: string, messageId: string) {
    setLastSeen(dmKey(barePeerJid(peerJid)), messageId);
  }

  return {
    firstUnseenId,
    latestRemoteMessageId,
    markDisplayed,
    persistLastSeen,
  };
}
