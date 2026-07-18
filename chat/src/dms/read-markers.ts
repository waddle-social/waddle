import { computed, ref, type ComputedRef, type Ref } from "vue";
import type { BrowserXmppClient, DmConversationScope } from "@/lib/xmpp-client";
import { dmConversationIdentityKey } from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import { dmKey, setLastSeen } from "@/lib/last-seen-store";
import { latestRemoteMessageIdFor } from "@/lib/timeline-state";
import { useReadReceiptPreference } from "@/preferences/read-receipts";
import { dmThreadRefFromMessage } from "@/dms/threading";
import { reportDisplayedMarkerFailure } from "@/lib/telemetry";

// XEP-0333 chat-marker outbound state for the DM side. Mirrors
// useChannelReadMarkers; uses the exact conversation identity as the
// last-seen-store key instead of the channel's roomKey.

type UseDmReadMarkersDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
  conversationScope?: (peerJid: string) => DmConversationScope;
};
type MarkDisplayedOptions = {
  syncMds?: boolean;
};

export function useDmReadMarkers(deps: UseDmReadMarkersDeps) {
  const { xmppClient, activePeerJid, messages, conversationScope } = deps;

  const firstUnseenId = ref<string | null>(null);
  const { sendDisplayedMarkers } = useReadReceiptPreference();
  const latestRemoteMessageId: ComputedRef<string | null> = computed(() =>
    latestRemoteMessageIdFor(messages.value),
  );

  function conversationId(peerJid: string): string {
    const client = xmppClient.value;
    const scope = conversationScope?.(peerJid)
      ?? (client?.isMucPmPeer?.(peerJid) === true ? "muc-occupant" : "account");
    return dmConversationIdentityKey(peerJid, () => scope === "muc-occupant");
  }

  function markDisplayed(messageId: string, options: MarkDisplayedOptions = {}) {
    if (!xmppClient.value || !activePeerJid.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.id ?? messageId;
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (sendDisplayedMarkers.value && target?.displayedMarkerRequested) {
      const scope = conversationScope?.(peerJid);
      void client
        .sendDmDisplayed(peerJid, targetId, dmThreadRefFromMessage(target), scope)
        .catch(() => reportDisplayedMarkerFailure({
          direction: "send",
          kind: "dm",
          reason: "send-failed",
          roundTripMs: elapsedSince(target.createdAt),
        }));
    }
    // XEP-0490 §3 publish for the DM. The chat id is the peer bare
    // JID; `stanza_id_by` is the user's own server JID (carried on
    // the inbound message). Only fires when both are surfaced.
    const stanzaId = target?.stanzaId;
    const stanzaIdBy = target?.stanzaIdBy;
    if (options.syncMds !== false && stanzaId && stanzaIdBy) {
      void client
        .publishMdsDisplayed(conversationId(peerJid), stanzaId, stanzaIdBy)
        .catch(() => undefined);
    }
  }

  function persistLastSeen(peerJid: string, messageId: string, capturedConversationId?: string) {
    setLastSeen(dmKey(capturedConversationId ?? conversationId(peerJid)), messageId);
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
