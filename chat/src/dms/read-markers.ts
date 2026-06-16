import { computed, ref, type ComputedRef, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { dmKey, setLastSeen } from "@/lib/last-seen-store";
import { latestRemoteMessageIdFor } from "@/lib/timeline-state";
import { useReadReceiptPreference } from "@/preferences/read-receipts";
import { dmThreadRefFromMessage } from "@/dms/threading";

// XEP-0333 chat-marker outbound state for the DM side. Mirrors
// useChannelReadMarkers; uses `dmKey(barePeerJid(peerJid))` as the
// last-seen-store key instead of the channel's roomKey.

type UseDmReadMarkersDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
};
type MarkDisplayedOptions = {
  syncMds?: boolean;
};

export function useDmReadMarkers(deps: UseDmReadMarkersDeps) {
  const { xmppClient, activePeerJid, messages } = deps;

  const firstUnseenId = ref<string | null>(null);
  const { sendDisplayedMarkers } = useReadReceiptPreference();
  const latestRemoteMessageId: ComputedRef<string | null> = computed(() =>
    latestRemoteMessageIdFor(messages.value),
  );

  function markDisplayed(messageId: string, options: MarkDisplayedOptions = {}) {
    if (!xmppClient.value || !activePeerJid.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.id ?? messageId;
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (sendDisplayedMarkers.value && target?.displayedMarkerRequested) {
      void client
        .sendDmDisplayed(peerJid, targetId, dmThreadRefFromMessage(target))
        .catch(() => undefined);
    }
    // XEP-0490 §3 publish for the DM. The chat id is the peer bare
    // JID; `stanza_id_by` is the user's own server JID (carried on
    // the inbound message). Only fires when both are surfaced.
    const stanzaId = target?.stanzaId;
    const stanzaIdBy = target?.stanzaIdBy;
    if (options.syncMds !== false && stanzaId && stanzaIdBy) {
      void client
        .publishMdsDisplayed(barePeerJid(peerJid), stanzaId, stanzaIdBy)
        .catch(() => undefined);
    }
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
