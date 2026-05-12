import type { LiveRoomMessage } from "@/lib/xmpp-client";
import type { MarkupSpan, MessageReference } from "@/lib/chat-ui";

// Typed-dispatch classifier for the channel-side omnibus inbound message
// handler. The wasm client currently exposes a single `setMessageHandler`
// callback that delivers every kind of room-bound stanza (regular message,
// XEP-0308 correction, XEP-0424 retraction, XEP-0425 moderator retract);
// the pre-refactor code branched on `msg.replacesId` / `msg.retractsId`
// inside the handler body.
//
// Reaction (XEP-0444) and displayed (XEP-0333) events arrive on their own
// typed handlers (`setReactionHandler` / `setDisplayedHandler`) — they
// never flow through `setMessageHandler` and so are deliberately excluded
// from this classifier's union.
//
// This classifier lifts the branch into a pure helper so `useLiveMerge`
// can switch on a tagged union instead of sniffing stanza properties. The
// classifier inspects ONLY the stanza shape — active-room filtering,
// notification routing, and chat-state typing all stay in the orchestrator
// because they depend on context the classifier doesn't see.
//
// A follow-up issue (filed from #351's plan) tracks the deeper fix:
// migrating `BrowserXmppClient` itself to emit typed events
// (`onRoomRetraction`, `onRoomReaction`, etc.) so the classifier becomes
// redundant and the channel side ends up as typed as the DM side already is.

// Inline trivial helper — pre-refactor this lived in
// channels/message-timeline-state, but importing UI-layer modules from
// lib/xmpp violates the dependency direction. The computation is four
// lines of pure stanza shaping; lifting it here keeps the protocol layer
// independent of channels/*.
function mucCorrectionSender(msg: Pick<LiveRoomMessage, "roomJid" | "nick" | "authorRealJid">): {
  authorJid: string;
  authorRealJid?: string;
} {
  return {
    authorJid: `${msg.roomJid}/${msg.nick}`,
    ...(msg.authorRealJid ? { authorRealJid: msg.authorRealJid } : {}),
  };
}

type ClassifiedRoomMessage =
  | {
      kind: "retraction";
      retractsId: string;
      retractionId?: string;
      moderationTargetId?: string;
      sender: { authorJid: string; authorRealJid?: string };
      raw: LiveRoomMessage;
    }
  | {
      kind: "correction";
      replacesId: string;
      body: string;
      sender: { authorJid: string; authorRealJid?: string };
      markup?: MarkupSpan[];
      references?: MessageReference[];
      extensionAnnotations?: LiveRoomMessage["extensionAnnotations"];
      extensionBodyFallback?: boolean;
    }
  | { kind: "live"; raw: LiveRoomMessage }
  | { kind: "ignore" };

export function classifyRoomMessage(msg: LiveRoomMessage): ClassifiedRoomMessage {
  if (msg.type !== "message") return { kind: "ignore" };

  // Retraction wins over correction when both are set: deleting a message
  // takes precedence over editing it. The pre-refactor handler had this
  // ordering implicitly via two sequential `if (msg.retractsId)` /
  // `if (msg.replacesId)` blocks; the classifier makes it explicit.
  if (msg.retractsId) {
    const out: ClassifiedRoomMessage = {
      kind: "retraction",
      retractsId: msg.retractsId,
      sender: mucCorrectionSender(msg),
      raw: msg,
    };
    if (msg.retractionId) out.retractionId = msg.retractionId;
    if (msg.moderationTargetId) out.moderationTargetId = msg.moderationTargetId;
    return out;
  }

  if (msg.replacesId) {
    const out: ClassifiedRoomMessage = {
      kind: "correction",
      replacesId: msg.replacesId,
      body: msg.body,
      sender: mucCorrectionSender(msg),
    };
    if (msg.markup && msg.markup.length > 0) out.markup = msg.markup;
    if (msg.references && msg.references.length > 0) out.references = msg.references;
    if (msg.extensionAnnotations && msg.extensionAnnotations.length > 0) {
      out.extensionAnnotations = msg.extensionAnnotations;
    }
    if (msg.extensionBodyFallback) out.extensionBodyFallback = true;
    return out;
  }

  return { kind: "live", raw: msg };
}
