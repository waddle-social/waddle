import { watch, type Ref } from "vue";

type ReadReceiptsActiveKind = "channel" | "dm" | null;

interface UseReadReceiptsOptions {
  isWindowFocused: Readonly<Ref<boolean>>;
  isPinnedAtEdge: Readonly<Ref<boolean>>;
  activeKind: Readonly<Ref<ReadReceiptsActiveKind>>;
  activeRoomJid: Readonly<Ref<string | null>>;
  activePeerJid: Readonly<Ref<string | null>>;
  latestRemoteMessageId: Readonly<Ref<string | null>>;
  unreadCountForActive: Readonly<Ref<number>>;
  markChannelRead: (roomJid: string) => void;
  markDmRead: (peerJid: string) => void;
  markDisplayed: (messageId: string) => void;
}

export function useReadReceipts(options: UseReadReceiptsOptions) {
  let lastDispatchedDisplayedId: string | null = null;
  let lastReadActiveKey: string | null = null;

  function activeKey(): string | null {
    const kind = options.activeKind.value;
    if (kind === "channel") {
      const jid = options.activeRoomJid.value;
      return jid ? `channel:${jid}` : null;
    }
    if (kind === "dm") {
      const jid = options.activePeerJid.value;
      return jid ? `dm:${jid}` : null;
    }
    return null;
  }

  watch(
    () => ({
      kind: options.activeKind.value,
      roomJid: options.activeRoomJid.value,
      peerJid: options.activePeerJid.value,
      focused: options.isWindowFocused.value,
      pinned: options.isPinnedAtEdge.value,
      latestId: options.latestRemoteMessageId.value,
      unread: options.unreadCountForActive.value,
    }),
    (state) => {
      const key = activeKey();
      if (key !== lastReadActiveKey) {
        lastDispatchedDisplayedId = null;
        lastReadActiveKey = key;
      }

      if (!state.focused || !state.pinned) return;
      if (!state.kind) return;

      const target = state.kind === "channel" ? state.roomJid : state.peerJid;
      if (!target) return;

      // XEP-0333 displayed markers need a message id to point at, so they
      // only fire when latestId is known and has advanced. Inbox mark-read
      // (urn:waddle:inbox:0) doesn't take a message id — clear it whenever
      // the active conversation has unread, regardless of latestId. Without
      // this split, body-less server projections (reactions on own
      // messages, retractions, backlog older than MAM) would leave the
      // unread badge stuck.
      if (state.latestId && state.latestId !== lastDispatchedDisplayedId) {
        lastDispatchedDisplayedId = state.latestId;
        options.markDisplayed(state.latestId);
      }

      if (state.unread > 0) {
        if (state.kind === "channel") {
          options.markChannelRead(target);
        } else {
          options.markDmRead(target);
        }
      }
    },
    { immediate: true },
  );
}
