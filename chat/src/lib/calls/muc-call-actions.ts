import { beginMucCall, reportCallError, type RawIqSender } from "./call-store";
import { clearMucCallParticipant } from "./muc-call-presence";
import type { CallMedia } from "./types";

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
}: MucCallStartActionOptions): Promise<boolean> {
  if (isBusy()) return false;
  const sender = getSender();
  if (!sender) return false;
  setStarting(true);
  try {
    await ensureJoined?.();
    await beginMucCall(
      sender,
      roomJid,
      media,
      getSelfNick(),
      getExpectedMixerJid?.(),
      getSelfFullJid?.(),
    );
    return true;
  } catch (err) {
    reportCallError(err);
    return false;
  } finally {
    setStarting(false);
  }
}

/**
 * Hard-refresh recovery path for a MUC call this browser resource
 * still advertises via Muji presence, but whose local LiveKit/call
 * state was lost with the old JS context. We no longer have the
 * per-attempt Jingle SID, so the conformant thing we can do is clear
 * this occupant's XEP-0272 presence. The server echo will remove the
 * call indicator for everyone; after the leave marker is sent, the
 * local clear makes the refreshed tab responsive immediately.
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
  try {
    await sender.update_muji_presence(
      roomJid,
      selfNick,
      false,
      false,
      false,
    );
    clearMucCallParticipant(roomJid, selfNick, selfFullJid);
    return true;
  } catch (err) {
    reportCallError(err);
    return false;
  }
}
