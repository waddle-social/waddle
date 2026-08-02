import { $callState, beginMucCall, reportCallError, type RawIqSender } from "./call-store";
import { selfCallMuted } from "./call-mic-state";
import { LIVEKIT_JOIN_EXPIRY_SKEW_MS, liveKitJoinTokenExpiresAt } from "./dm-call-activity";
import { clearMucCallParticipant, normalizeMucCallRoomJid } from "./muc-call-presence";
import { selfRaisedHandFor, setSelfRaisedHand } from "./call-raised-hand";
import {
  forgetMucCallSession,
  markMucCallSessionTerminatePending,
  readMucCallSession,
} from "./muc-call-session-cache";
import type { CallMedia, LiveKitJoin } from "./types";
import {
  resumeCallAttempt,
  markCallAttemptAccepted,
  reportFailedCallAttempt,
} from "./call-lifecycle-telemetry";

type MucCallStartActionOptions = {
  roomJid: string;
  media: CallMedia;
  isBusy: () => boolean;
  setStarting: (starting: boolean) => void;
  getSender: () => RawIqSender | null;
  getSelfNick: () => string | undefined;
  getSelfFullJid?: () => string | null | undefined;
  getExpectedMixerJid?: () => string | null | undefined;
  ensureJoined?: () => Promise<void>;
  /**
   * When true, attempt a direct LiveKit reconnect via
   * `resumeMucCallActivity` before falling back to a fresh Jingle
   * `beginMucCall`. The rejoin affordance after a tab reload sets
   * this; the "join a call you weren't in" fresh-joiner button
   * leaves it false.
   */
  tryResumeFirst?: boolean;
};

type RetainedMucCallLeaveActionOptions = {
  roomJid: string;
  getSender: () => RawIqSender | null;
  getSelfNick: () => string | undefined;
  getSelfFullJid?: () => string | null | undefined;
};

/**
 * Shared action behind the group-call buttons. Kept outside the SFC
 * so tests can exercise the exact click handler semantics without
 * introducing a Vue test-utils dependency.
 */
export async function startMucCallAction({
  roomJid,
  media,
  isBusy,
  setStarting,
  getSender,
  getSelfNick,
  getSelfFullJid,
  getExpectedMixerJid,
  ensureJoined,
  tryResumeFirst = false,
}: MucCallStartActionOptions): Promise<boolean> {
  if (isBusy()) return false;
  const lifecycleAttemptId = crypto.randomUUID();
  const sender = getSender();
  if (!sender) {
    reportFailedCallAttempt(lifecycleAttemptId, "muc", roomJid);
    return false;
  }
  setStarting(true);
  try {
    await ensureJoined?.();
    if (tryResumeFirst) {
      const resumed = await resumeMucCallActivity({
        roomJid,
        getSender,
        getSelfNick,
        getSelfFullJid,
      });
      if (resumed) return true;
    }
    await beginMucCall(
      sender,
      roomJid,
      media,
      getSelfNick(),
      getExpectedMixerJid?.(),
      getSelfFullJid?.(),
      lifecycleAttemptId,
    );
    return true;
  } catch (err) {
    reportFailedCallAttempt(lifecycleAttemptId, "muc", roomJid);
    reportCallError(err);
    return false;
  } finally {
    setStarting(false);
  }
}

/**
 * Hard-refresh recovery path for a MUC call this browser resource
 * still advertises via Muji presence, but whose local LiveKit/call
 * state was lost with the old JS context. We clear this occupant's
 * XEP-0272 presence first, then send the cached per-attempt Jingle
 * `session-terminate` when available. That ordering follows
 * XEP-0272 §Leaving while still giving the SFU the XEP-0166
 * termination it expects.
 */
export async function leaveRetainedMucCallAction({
  roomJid,
  getSender,
  getSelfNick,
  getSelfFullJid,
}: RetainedMucCallLeaveActionOptions): Promise<boolean> {
  const sender = getSender();
  const selfNick = getSelfNick();
  if (!sender?.update_muji_presence || !selfNick) return false;
  const selfFullJid = getSelfFullJid?.() ?? null;
  const cachedSession = readMucCallSession({ roomJid, selfFullJid });
  let terminated = !cachedSession;
  try {
    await sender.update_muji_presence(roomJid, selfNick, false, false, false, {
      handRaised: false,
      muted: false, // leaving the call clears all in-call state
    });
    clearMucCallParticipant(roomJid, selfNick, selfFullJid, {
      includeAggregate: true,
    });
  } catch (err) {
    reportCallError(err);
    return false;
  }
  if (cachedSession) {
    try {
      if (!sender.send_muji_session_terminate) {
        throw new Error(
          "wasm client does not expose send_muji_session_terminate; rebuild the wasm bundle",
        );
      }
      await sender.send_muji_session_terminate(
        cachedSession.roomJid,
        cachedSession.sid,
        crypto.randomUUID(),
      );
      forgetMucCallSession({
        roomJid: cachedSession.roomJid,
        selfFullJid,
        sid: cachedSession.sid,
      });
      terminated = true;
    } catch (err) {
      markMucCallSessionTerminatePending({
        roomJid: cachedSession.roomJid,
        selfFullJid,
        sid: cachedSession.sid,
        media: cachedSession.media,
      });
      reportCallError(err);
    }
  }
  return terminated;
}

/**
 * Resume options for a recovered MUC group call. Mirror of
 * `resumeDmCallActivity`'s shape.
 */
type ResumeMucCallActionOptions = {
  roomJid: string;
  getSender: () => RawIqSender | null;
  getSelfNick: () => string | undefined;
  getSelfFullJid?: () => string | null | undefined;
  now?: Date;
};

/**
 * Test whether the cached MUC session for `(roomJid, selfFullJid)`
 * can be reused for a direct LiveKit reconnect without a fresh
 * Jingle session-initiate handshake.
 *
 * The check is strictly local: a cached entry is usable only when it
 * carries a `join`, the join's identity matches our current full JID
 * (LiveKit identity-uniqueness then displaces the pre-reload session
 * cleanly), and the JWT's `exp` claim sits comfortably ahead of
 * `now`. Per LiveKit's documented operational behavior the JWT's TTL
 * only gates the initial connect — once we reconnect, LK auto-rotates
 * 10-minute refresh tokens in-band — but we still require headroom
 * so a token that's seconds from expiry doesn't race the reconnect.
 */
export function canResumeMucCallActivity(options: {
  roomJid: string;
  selfFullJid: string | null | undefined;
  now?: Date;
}): boolean {
  const selfFullJid = options.selfFullJid?.trim() ?? "";
  if (!selfFullJid) return false;
  const session = readMucCallSession({
    roomJid: options.roomJid,
    selfFullJid,
    now: options.now,
  });
  if (!session || session.terminatePending) return false;
  if (!session.join) return false;
  if (session.join.identity !== selfFullJid) return false;
  const expiresAt = liveKitJoinTokenExpiresAt(session.join.token);
  const now = options.now ?? new Date();
  if (!expiresAt) return false;
  return expiresAt.getTime() > now.getTime() + LIVEKIT_JOIN_EXPIRY_SKEW_MS;
}

/**
 * Hard-refresh recovery path for a MUC call this browser resource
 * was actively in before the tab reload.
 *
 * Reads the persisted `LiveKitJoin` from the MUC session cache and
 * promotes `$callState` directly to `active` with the cached
 * credentials. The `CallOverlay` watcher (keyed on
 * `state.value.sid`) then drives `engine.connect(active.join,
 * active.media)`, which reconnects to the same LiveKit room as the
 * same identity — LK's identity-uniqueness invariant kicks any
 * orphan session left behind by the dead pre-reload resource.
 *
 * Critically, this path does NOT mint a fresh Jingle attempt: no
 * new `attemptId`, no new SFU session, no doubled Muji presence.
 * The pre-reload Muji=active presence is still in the MUC roster
 * and remains the authoritative advertisement; explicit hangup
 * clears it via the normal `tearDownActiveCall` path. If the cached
 * token cannot be used (missing/expired/identity-mismatch) the
 * caller falls back to `beginMucCall`.
 *
 * Best-effort re-publishes our Muji=active presence after promoting
 * `$callState`: harmless when the server still has our previous
 * presence (the rebroadcast is a no-op update from the room's
 * perspective) but useful when SM-resume changed our resource and
 * the room hasn't yet seen the new resource's Muji.
 */
export async function resumeMucCallActivity({
  roomJid,
  getSender,
  getSelfNick,
  getSelfFullJid,
  now,
}: ResumeMucCallActionOptions): Promise<boolean> {
  const current = $callState.get();
  if (current.phase !== "idle" && current.phase !== "ended") return false;

  const selfFullJid = getSelfFullJid?.()?.trim() ?? "";
  if (!selfFullJid) return false;
  if (!canResumeMucCallActivity({ roomJid, selfFullJid, now })) return false;

  const session = readMucCallSession({ roomJid, selfFullJid, now });
  if (!session || !session.join || !session.media) return false;

  const selfNick = getSelfNick();
  if (!selfNick) return false;

  const media: CallMedia = session.media;
  const join: LiveKitJoin = session.join;

  $callState.set({
    phase: "active",
    peer: session.roomJid,
    sid: session.sid,
    media,
    join,
    kind: "muc",
    selfNick,
    selfFullJid,
  });
  resumeCallAttempt(session.sid, "muc", session.roomJid);
  markCallAttemptAccepted(session.sid);

  // Best-effort republish of Muji active presence under the current
  // resource. Non-fatal — the LK→Muji webhook bridge will reconcile
  // even if this rebroadcast races a transient send failure.
  const sender = getSender();
  if (sender?.update_muji_presence) {
    try {
      await sender.update_muji_presence(session.roomJid, selfNick, true, false, media.video, {
        handRaised: selfRaisedHandFor(session.roomJid), // preserve across resume
        muted: selfCallMuted(), // preserve across resume
      });
    } catch (err) {
      reportCallError(err);
    }
  }

  return true;
}

/**
 * Raise or lower this client's hand in the active MUC call (#1029).
 *
 * The raised-hand state is a XEP-0045 presence extension carried
 * alongside `<muji/>`, so "setting" it means re-emitting our current
 * active call presence with the flag toggled — the wasm FFI builds the
 * `<in-call><hand-raised/></in-call>` child when `raised` is true and
 * omits it (lowering for everyone) when false. We optimistically record
 * the local intent first so the control bar flips immediately and any
 * later media re-emit preserves the flag; a send failure reverts it.
 *
 * No-op (returns `false`) unless we're in an active MUC call with a
 * known nick and a wasm client that exposes the presence FFI.
 */
export async function setMucCallHandRaised(
  sender: RawIqSender,
  raised: boolean,
): Promise<boolean> {
  const state = $callState.get();
  if (state.phase !== "active" || state.kind !== "muc" || !state.selfNick) {
    return false;
  }
  if (!sender.update_muji_presence) return false;
  const roomJid = normalizeMucCallRoomJid(state.peer);
  if (!roomJid) return false;

  setSelfRaisedHand(roomJid, raised);
  try {
    await sender.update_muji_presence(roomJid, state.selfNick, true, false, state.media.video, {
      handRaised: raised,
      // re-stamp the current mute so a hand toggle never drops the
      // `<muted/>` marker (presence is last-writer-wins)
      muted: selfCallMuted(),
    });
    return true;
  } catch (err) {
    // Revert the optimistic flip — but only if a newer toggle hasn't
    // already superseded it (rapid click / double tap), so we don't
    // clobber the latest user intent with this stale request's rollback.
    if (selfRaisedHandFor(roomJid) === raised) {
      setSelfRaisedHand(roomJid, !raised);
    }
    reportCallError(err);
    return false;
  }
}

/**
 * Broadcast this client's mute state to the active MUC call as
 * `urn:waddle:in-call:0` presence (#1030). Mirrors
 * [`setMucCallHandRaised`]: "setting" mute means re-emitting our current
 * active call presence with the `<muted/>` marker toggled — the wasm FFI
 * builds the `<in-call>` child when `muted` is true and omits it (unmuting
 * for everyone) when false. Re-stamps the CURRENT raised hand so a mute
 * toggle never drops it.
 *
 * The local mic toggle (`$callMicEnabled`) is the source of truth and is
 * already flipped by the caller, so there is no optimistic state to roll
 * back here — a failed broadcast just leaves peers momentarily stale until
 * the next presence, and is reported.
 *
 * No-op (returns `false`) unless we're in an active MUC call with a known
 * nick and a wasm client that exposes the presence FFI.
 */
export async function broadcastMucCallSelfMute(
  sender: RawIqSender,
  muted: boolean,
): Promise<boolean> {
  const state = $callState.get();
  if (state.phase !== "active" || state.kind !== "muc" || !state.selfNick) {
    return false;
  }
  if (!sender.update_muji_presence) return false;
  const roomJid = normalizeMucCallRoomJid(state.peer);
  if (!roomJid) return false;

  try {
    await sender.update_muji_presence(roomJid, state.selfNick, true, false, state.media.video, {
      handRaised: selfRaisedHandFor(roomJid), // preserve across a mute toggle
      muted,
    });
    return true;
  } catch (err) {
    reportCallError(err);
    return false;
  }
}

/**
 * Re-emit the active MUC call's full XEP-0272 advertisement (with the
 * current hand/mute markers). A fresh XMPP bind wiped our server-side
 * occupant, and the plain rejoin presence recreates it WITHOUT
 * `<muji/>` — other occupants would show a still-running call as ended
 * for its remainder. Called after the call room's rejoin confirms
 * (#1621 review round 2).
 */
export async function readvertiseMucCallPresence(sender: RawIqSender): Promise<boolean> {
  const state = $callState.get();
  if (state.phase !== "active" || state.kind !== "muc" || !state.selfNick) {
    return false;
  }
  if (!sender.update_muji_presence) return false;
  const roomJid = normalizeMucCallRoomJid(state.peer);
  if (!roomJid) return false;
  try {
    await sender.update_muji_presence(roomJid, state.selfNick, true, false, state.media.video, {
      handRaised: selfRaisedHandFor(roomJid),
      muted: selfCallMuted(),
    });
    return true;
  } catch (err) {
    reportCallError(err);
    return false;
  }
}
