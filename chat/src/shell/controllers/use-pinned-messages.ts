import { type ComputedRef, type Ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid } from "@/lib/xmpp-client";
import { $pinnedRooms, hydratePinnedRoom, pinnedRoomsEpoch } from "@/stores/pinned-messages";
import { hydratePinnedBodiesOnPanelOpen, hydrateSinglePinnedBody } from "@/services/pinned-message-bodies";
import { trustedLinkPreviewMediaOrigin } from "@/lib/xmpp/link-preview";
import { dmMessageFromArchived, roomMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import { mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import { fromLiveDmMessage } from "@/dms/message-timeline-state";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { ContentAreaHandle } from "@/shell/controllers/use-active-conversation";

interface PinnedMessagesDeps {
  ui: ChatShellState;
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  session: ComputedRef<WaddleSession | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  isActiveDirectDmSurface: () => boolean;
  activeTarget: ComputedRef<ReturnType<typeof useChannelMessages> | ReturnType<typeof useDirectMessages>>;
  contentAreaRef: Ref<ContentAreaHandle | null>;
}

/**
 * Pinned-message support (#414): pin/unpin actions on the active surface,
 * jump-to-pin, and the hydration watches that fetch pinned message bodies
 * when the panel opens or new pin events arrive.
 */
export function usePinnedMessages(deps: PinnedMessagesDeps) {
  const {
    ui,
    xmppClient,
    session,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    isActiveDirectDmSurface,
    activeTarget,
    contentAreaRef,
  } = deps;

  const pinnedRooms = useStore($pinnedRooms);

  /** #414: pin or unpin the targeted message in the active channel. The
   * server gates on Owner/Admin affiliation; the action sheet entry is
   * also visibility-gated client-side, so non-admins shouldn't reach
   * this — but the server is authoritative. */
  function pinActiveMessage(messageId: string) {
    const client = xmppClient.value;
    if (!client) return;
    const stanzaId = resolvePinTargetStanzaId(messageId);
    if (!stanzaId) return;
    if (isActiveDirectDmSurface()) {
      const peer = dmConversations.activePeerJid.value;
      if (!peer || !("pinDirectMessage" in client)) return;
      void client.pinDirectMessage(peer, stanzaId).catch((error: unknown) => {
        console.warn("pinDirectMessage failed", error);
      });
      return;
    }
    const channel = waddles.currentChannel.value;
    if (!channel) return;
    void client.pinMessage(channel.spaceId ?? "", channel.id, stanzaId).catch((error: unknown) => {
      console.warn("pinMessage failed", error);
    });
  }

  function unpinActiveMessage(messageId: string) {
    const client = xmppClient.value;
    if (!client) return;
    const stanzaId = resolvePinTargetStanzaId(messageId);
    if (!stanzaId) return;
    if (isActiveDirectDmSurface()) {
      const peer = dmConversations.activePeerJid.value;
      if (!peer || !("unpinDirectMessage" in client)) return;
      void client.unpinDirectMessage(peer, stanzaId).catch((error: unknown) => {
        console.warn("unpinDirectMessage failed", error);
      });
      return;
    }
    const channel = waddles.currentChannel.value;
    if (!channel) return;
    void client.unpinMessage(channel.spaceId ?? "", channel.id, stanzaId).catch((error: unknown) => {
      console.warn("unpinMessage failed", error);
    });
  }

  /** #414: jump to a pinned message from the panel — load it into the
   * timeline if needed, then scroll/center. Stanza-id is the room
   * archive id; message-id used in the timeline matches via wireIds /
   * reactionTargetId. The chat client's existing
   * `scrollToMessage(messageId)` accepts the wire id; we route the
   * stanza-id directly since `ensureMessageLoaded` resolves both. */
  async function jumpToPinnedMessage(stanzaId: string) {
    await activeTarget.value.ensureMessageLoaded(stanzaId);
    await contentAreaRef.value?.scrollToMessage(stanzaId);
  }

  /** Map a chat-side message id to the room's XEP-0359 stanza-id. The
   * pin server expects the stable archive id stamped by-room, not the
   * wire `id` attribute or the client-assigned origin-id. Timeline
   * rows expose this as `reactionTargetId` (room messages) /
   * `replyableId` (DMs use this for reply-to); both pull from
   * `message.stanza_id` upstream. Returns null when no archive id is
   * known yet (e.g., a queued send hasn't been reflected). */
  function resolvePinTargetStanzaId(messageId: string): string | null {
    const message = activeTarget.value.messages.value.find((m) => m.id === messageId);
    if (!message) return null;
    const m = message as TimelineMessage & {
      reactionTargetId?: string;
      replyableId?: string;
    };
    return m.reactionTargetId ?? m.replyableId ?? null;
  }

  // Waddle MAM stanza-id filter: on false → true transition, batch-fetch
  // any pinned stanza-ids not already in the loaded timeline or cache.
  watch(() => ui.showPinnedPanel.value, async (open) => {
    if (!open) return;
    if (isActiveDirectDmSurface()) return;
    const client = xmppClient.value;
    const spaceId = waddles.currentChannel.value?.spaceId ?? "";
    const channelId = waddles.activeChannelId.value;
    const roomJid = messaging.currentRoomJid.value;
    if (!client || !channelId || !roomJid) return;
    if (!("fetchRoomMessagesByStanzaIds" in client)) return;
    const convertForTimeline = (a: Parameters<typeof roomMessageFromArchived>[0]) => {
      const live = roomMessageFromArchived(a, {
        trustedMediaOrigin: session.value
          ? trustedLinkPreviewMediaOrigin(session.value)
          : null,
      });
      return live && session.value
        ? mapLiveRoomMessageToTimeline(session.value, live)
        : null;
    };
    try {
      await hydratePinnedBodiesOnPanelOpen({
        fetchByStanzaIds: (stanzaIds) =>
          client.fetchRoomMessagesByStanzaIds(spaceId, channelId, stanzaIds),
        spaceId,
        channelId,
        roomJid,
        timelineMessages: messaging.messages.value,
        convert: convertForTimeline,
      });
    } catch (error) {
      console.warn("hydratePinnedBodiesOnPanelOpen failed", error);
    }
  });
  watch(() => ui.showPinnedPanel.value, async (open) => {
    if (!open || !isActiveDirectDmSurface()) return;
    const client = xmppClient.value;
    const peerJid = dmConversations.activePeerJid.value;
    const currentSession = session.value;
    if (!client || !peerJid || !currentSession) return;
    if (!("fetchDirectMessagesByStanzaIds" in client)) return;
    const convertForTimeline = (archived: Parameters<typeof dmMessageFromArchived>[0]) => {
      const live = dmMessageFromArchived(archived, barePeerJid(currentSession.jid), {
        trustedMediaOrigin: trustedLinkPreviewMediaOrigin(currentSession),
      });
      return live ? fromLiveDmMessage(currentSession, live) : null;
    };
    try {
      await hydratePinnedBodiesOnPanelOpen({
        fetchByStanzaIds: (stanzaIds) =>
          client.fetchDirectMessagesByStanzaIds(peerJid, stanzaIds),
        spaceId: "",
        channelId: "",
        roomJid: peerJid,
        timelineMessages: dmMessaging.messages.value,
        convert: convertForTimeline,
      });
    } catch (error) {
      console.warn("hydratePinnedBodiesOnPanelOpen failed", error);
    }
  });
  watch(pinnedRooms, (rooms) => {
    if (!ui.showPinnedPanel.value || !isActiveDirectDmSurface()) return;
    const client = xmppClient.value;
    const peerJid = dmConversations.activePeerJid.value;
    const currentSession = session.value;
    if (!client || !peerJid || !currentSession) return;
    if (!("fetchDirectMessagesByStanzaIds" in client)) return;
    const state = rooms.get(peerJid);
    const entry = state?.entries[0];
    if (!entry) return;
    const convertForTimeline = (archived: Parameters<typeof dmMessageFromArchived>[0]) => {
      const live = dmMessageFromArchived(archived, barePeerJid(currentSession.jid), {
        trustedMediaOrigin: trustedLinkPreviewMediaOrigin(currentSession),
      });
      return live ? fromLiveDmMessage(currentSession, live) : null;
    };
    void hydrateSinglePinnedBody({
      fetchByStanzaIds: (stanzaIds) =>
        client.fetchDirectMessagesByStanzaIds(peerJid, stanzaIds),
      spaceId: "",
      channelId: "",
      roomJid: peerJid,
      stanzaId: entry.target_stanza_id,
      timelineMessages: dmMessaging.messages.value,
      convert: convertForTimeline,
    }).catch((error) => console.warn("hydrateSinglePinnedBody failed", error));
  });
  watch(
    [xmppClient, () => ui.sidebarMode.value, () => dmConversations.activePeerJid.value],
    ([client, mode, peerJid]) => {
      if (mode !== "dms" || !peerJid) return;
      if (!client || !("fetchDirectPins" in client)) {
        hydratePinnedRoom(peerJid, []);
        return;
      }
      const epoch = pinnedRoomsEpoch();
      void client.fetchDirectPins(peerJid)
        .then((entries) => {
          if (!isActiveDirectDmSurface() || dmConversations.activePeerJid.value !== peerJid) return;
          hydratePinnedRoom(peerJid, entries, epoch);
        })
        .catch((error: unknown) => {
          console.warn("fetchDirectPins failed", error);
          hydratePinnedRoom(peerJid, [], epoch);
        });
    },
    { immediate: true },
  );

  return {
    pinActiveMessage,
    unpinActiveMessage,
    jumpToPinnedMessage,
  };
}
