import {
  $callState,
  beginOutgoingCall,
  reportCallError,
  scheduleOutgoingTimeout,
} from "./call-store";
import { clearDmCallActivity } from "./dm-call-activity";
import {
  newCallSid,
  outboundCalls,
  type CallWireSender,
} from "./outbound";
import type { CallMedia } from "./types";

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
