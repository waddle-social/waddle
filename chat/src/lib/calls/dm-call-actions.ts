import {
  $callState,
  acceptIncomingTieBreakPropose,
  beginOutgoingCall,
  reportCallError,
  scheduleOutgoingTimeout,
} from "./call-store";
import { canResumeDmCallActivity, clearDmCallActivity, readDmCallActivity } from "./dm-call-activity";
import {
  newCallSid,
  outboundCalls,
  type CallWireSender,
} from "./outbound";
import type { CallMedia } from "./types";
import { barePeerJid } from "../xmpp/jid";

export async function startDmCallAction(options: {
  peerBareJid: string;
  media: CallMedia;
  getSender: () => CallWireSender | null;
  getInitiator?: () => string | undefined;
}): Promise<void> {
  const current = $callState.get();
  if (current.phase !== "idle" && current.phase !== "ended") return;

  const sender = options.getSender();
  if (!sender) return;

  const sid = newCallSid();
  beginOutgoingCall(
    options.peerBareJid,
    sid,
    options.media,
    options.getInitiator?.(),
  );

  try {
    await outboundCalls.propose(sender, options.peerBareJid, sid, options.media);
    scheduleOutgoingTimeout(sender, sid);
  } catch (err) {
    const next = $callState.get();
    if (next.phase === "outgoing" && next.sid === sid) {
      $callState.set({ phase: "idle" });
    }
    clearDmCallActivity(options.peerBareJid, sid);
    reportCallError(err);
  }
}

export async function answerIncomingDmCallActivity(options: {
  peerBareJid: string;
  proposerFullJid: string;
  sid: string;
  media: CallMedia;
  getSender: () => CallWireSender | null;
}): Promise<void> {
  const peer = barePeerJid(options.peerBareJid).toLowerCase();
  const proposer = options.proposerFullJid.trim();
  if (!peer || barePeerJid(proposer).toLowerCase() !== peer) return;

  const sender = options.getSender();
  if (!sender) return;

  const current = $callState.get();
  const canUseCurrentIncoming = current.phase === "incoming" &&
    current.sid === options.sid &&
    barePeerJid(current.from).toLowerCase() === peer;
  const needsHydratedIncoming = current.phase === "idle" || current.phase === "ended";
  if (!canUseCurrentIncoming && !needsHydratedIncoming) return;
  const activity = needsHydratedIncoming ? readDmCallActivity(peer) : null;
  if (needsHydratedIncoming && (
    !activity ||
    activity.sid !== options.sid ||
    activity.state !== "ringing" ||
    activity.direction !== "incoming" ||
    !activity.remoteFullJid
  )) return;
  const responseTarget = canUseCurrentIncoming ? current.from : activity?.remoteFullJid ?? proposer;

  if (needsHydratedIncoming) {
    acceptIncomingTieBreakPropose({
      kind: "propose",
      from: responseTarget,
      sid: options.sid,
      media: activity?.media ?? options.media,
    });
  } else if (canUseCurrentIncoming) {
    $callState.set({ ...current, accepting: true });
  }
  if (needsHydratedIncoming) {
    const next = $callState.get();
    if (next.phase === "incoming" && next.sid === options.sid) {
      $callState.set({ ...next, accepting: true });
    }
  }

  try {
    await outboundCalls.proceed(sender, responseTarget, options.sid);
  } catch (err) {
    const next = $callState.get();
    if (needsHydratedIncoming && next.phase === "incoming" && next.sid === options.sid) {
      $callState.set({ phase: "idle" });
    } else if (canUseCurrentIncoming && next.phase === "incoming" && next.sid === options.sid) {
      $callState.set({ ...next, accepting: false });
    }
    reportCallError(err);
  }
}

export function resumeDmCallActivity(options: {
  peerBareJid: string;
  getSelfFullJid?: () => string | undefined;
  now?: Date;
}): boolean {
  const current = $callState.get();
  if (current.phase !== "idle" && current.phase !== "ended") return false;

  const peer = barePeerJid(options.peerBareJid).toLowerCase();
  if (!peer) return false;

  const activity = readDmCallActivity(peer, options.now);
  const selfFullJid = options.getSelfFullJid?.()?.trim() ?? "";
  if (!activity || !canResumeDmCallActivity(activity, selfFullJid, options.now)) {
    return false;
  }

  $callState.set({
    phase: "active",
    kind: "dm",
    peer: activity.remoteFullJid,
    sid: activity.sid,
    media: activity.media,
    join: activity.join,
    initiator: selfFullJid,
  });
  return true;
}
