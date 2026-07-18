import {
  $callState,
  acceptIncomingTieBreakPropose,
  beginOutgoingCall,
  reportCallError,
  scheduleOutgoingTimeout,
} from "./call-store";
import {
  canResumeDmCallActivity,
  clearDmCallActivity,
  readDmCallActivity,
  readEndableDmCallActivity,
} from "./dm-call-activity";
import {
  newCallSid,
  outboundCalls,
  type CallWireSender,
} from "./outbound";
import type { CallMedia } from "./types";
import { barePeerJid } from "../xmpp/jid";
import {
  resumeCallAttempt,
  finishCallAttempt,
  markCallAttemptAccepted,
  reportFailedCallAttempt,
} from "./call-lifecycle-telemetry";

export async function startDmCallAction(options: {
  peerBareJid: string;
  media: CallMedia;
  getSender: () => CallWireSender | null;
  getInitiator?: () => string | undefined;
}): Promise<void> {
  const current = $callState.get();
  if (current.phase !== "idle" && current.phase !== "ended") return;

  const sid = newCallSid();
  const sender = options.getSender();
  if (!sender) {
    reportFailedCallAttempt(sid, "dm");
    return;
  }

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
      finishCallAttempt(sid, { setupOutcome: "failed", endReason: "error" });
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
      finishCallAttempt(options.sid, { setupOutcome: "failed", endReason: "error" });
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

  const selfFullJid = options.getSelfFullJid?.()?.trim() ?? "";
  const activity = readDmCallActivity(peer, options.now, selfFullJid);
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
  resumeCallAttempt(activity.sid, "dm");
  markCallAttemptAccepted(activity.sid);
  return true;
}

export async function endRecoveredDmCallAction(options: {
  peerBareJid: string;
  sid?: string | null;
  getSender: () => CallWireSender | null;
  getSelfFullJid?: () => string | undefined;
  now?: Date;
}): Promise<boolean> {
  const peer = barePeerJid(options.peerBareJid).toLowerCase();
  if (!peer) return false;

  const sender = options.getSender();
  if (!sender) return false;

  const selfFullJid = options.getSelfFullJid?.()?.trim() ?? "";
  const activity = readEndableDmCallActivity(peer, selfFullJid, options.sid, options.now);
  if (!activity) {
    return false;
  }
  const remoteFullJid = activity.remoteFullJid;
  if (!remoteFullJid) return false;

  let sentEndMarker = false;
  let terminateAllowsFinish = false;
  try {
    const outcome = await outboundCalls.sessionTerminateWithOutcome(
      sender,
      remoteFullJid,
      activity.sid,
      "success",
    );
    terminateAllowsFinish = outcome === "ok" || outcome === "orphaned";
    if (outcome === "error") {
      reportCallError(new Error("call session terminate failed"));
    }
  } catch (err) {
    reportCallError(err);
  }

  if (terminateAllowsFinish) {
    try {
      await outboundCalls.finish(sender, remoteFullJid, activity.sid);
      sentEndMarker = true;
    } catch (err) {
      reportCallError(err);
    }
  }

  if (!sentEndMarker) return false;

  clearDmCallActivity(peer, activity.sid);
  return true;
}
