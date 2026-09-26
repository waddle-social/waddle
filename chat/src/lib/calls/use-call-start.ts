import { computed, ref, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import { $callState, type RawIqSender } from "./call-store";
import { startDmCallAction } from "./dm-call-actions";
import { useDmCallActivity } from "./dm-call-activity";
import { canResumeMucCallActivity, startMucCallAction } from "./muc-call-actions";
import type { CallWireSender } from "./outbound";
import type { CallMedia } from "./types";
import { useRoomHasActiveCall } from "./use-active-muc-call";
import { connectionStore } from "@/lib/connection-store";
import { jidDomain } from "@/lib/xmpp/jid";

/**
 * Start-call wiring shared by the header call buttons and the compact
 * header's overflow menu, so both surfaces start calls through the same
 * XEP-0353 (DM) and XEP-0272 Muji (MUC) paths.
 */

function wasmClient<T>(): T | null {
  // BrowserXmppClient stores the wasm client as `xmpp`; we cast
  // through unknown because the field is private on the class.
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as T | undefined) ?? null;
}

function selfFullJid(): string | undefined {
  return connectionStore.selfFullJid ??
    (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid;
}

/** True while any call is ringing or live, which blocks starting another. */
function useInCall(): ComputedRef<boolean> {
  const state = useStore($callState);
  return computed(() => state.value.phase !== "idle" && state.value.phase !== "ended");
}

export function useDmCallStart(peerBareJid: () => string | undefined): {
  /** False while the peer has call activity; the activity affordance
   *  (answer/reconnect) replaces the start controls. */
  canStart: ComputedRef<boolean>;
  busy: ComputedRef<boolean>;
  start: (media: CallMedia) => Promise<void>;
} {
  const inCall = useInCall();
  const { activity } = useDmCallActivity(peerBareJid);
  return {
    canStart: computed(() => !!peerBareJid() && !activity.value),
    busy: inCall,
    start: async (media) => {
      const peer = peerBareJid();
      if (!peer) return;
      await startDmCallAction({
        peerBareJid: peer,
        media,
        getSender: () => wasmClient<CallWireSender>(),
        getInitiator: selfFullJid,
      });
    },
  };
}

export function useMucCallStart(
  roomJid: () => string,
  options: { hideWhenActiveCall?: () => boolean } = {},
): {
  /** False when the room already has a live call and the caller asked
   *  the conversation banner to own the join affordance. */
  canStart: ComputedRef<boolean>;
  busy: ComputedRef<boolean>;
  start: (media: CallMedia) => Promise<void>;
  roomCall: ReturnType<typeof useRoomHasActiveCall>;
} {
  const inCall = useInCall();
  const starting = ref(false);
  const roomCall = useRoomHasActiveCall(roomJid);
  const busy = computed(() => inCall.value || starting.value);

  async function start(media: CallMedia): Promise<void> {
    const room = roomJid();
    if (!room) return;
    // Group call: the SFU mixer (`calls.<server-domain>`) mints the
    // room-scoped LiveKit token via a XEP-0272 Muji-bearing Jingle
    // session-initiate, publishes active Muji presence, then flips the
    // store to `active` so the overlay's LiveKit connect only starts
    // after the room-visible call indicator is valid. Our MUC nick is
    // the session username — also what `joinRoom` registered.
    await startMucCallAction({
      roomJid: room,
      media,
      isBusy: () => busy.value,
      setStarting: (next) => {
        starting.value = next;
      },
      getSender: () => wasmClient<RawIqSender>(),
      getSelfNick: () => connectionStore.session?.username ?? undefined,
      getSelfFullJid: selfFullJid,
      getExpectedMixerJid: () => {
        const accountJid = connectionStore.session?.jid;
        return accountJid ? `calls.${jidDomain(accountJid)}` : undefined;
      },
      ensureJoined: async () => {
        const client = connectionStore.client as unknown as {
          ensureJoined?: (roomJid: string) => Promise<void>;
        } | null;
        await client?.ensureJoined?.(room);
      },
      tryResumeFirst:
        roomCall.localResourceInCall.value ||
        canResumeMucCallActivity({
          roomJid: room,
          selfFullJid: selfFullJid() ?? null,
        }),
    });
  }

  return {
    canStart: computed(() =>
      !!roomJid() && !(options.hideWhenActiveCall?.() && roomCall.hasActiveCall.value),
    ),
    busy,
    start,
    roomCall,
  };
}
