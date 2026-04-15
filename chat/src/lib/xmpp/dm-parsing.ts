import type { ReceivedMessage } from "stanza/protocol";
import type {
  ChatStateType,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
} from "./types";
import { barePeerJid } from "./jid";
import { ext, extractMessageExtensions } from "./message-parsing";

function localpart(jid: string): string {
  return barePeerJid(jid).split("@")[0] ?? "unknown";
}

export interface DmHandlers {
  selfBareJid: string;
  onMessage: ((msg: LiveDmMessage) => void) | null;
  onChatState: ((event: DmChatStateEvent) => void) | null;
  onDisplayed: ((event: DmDisplayedEvent) => void) | null;
  onReaction: ((event: DmReactionEvent) => void) | null;
}

export function dispatchChat(msg: ReceivedMessage, h: DmHandlers): void {
  if (msg.type && msg.type !== "chat" && msg.type !== "normal") return;

  const fromBare = barePeerJid(msg.from ?? "");
  const toBare = barePeerJid(msg.to ?? "");
  if (!fromBare || !toBare) return;

  // Derive the MUC service domain from the user's own JID domain.
  const selfDomain = h.selfBareJid.split("@")[1] ?? "";
  const mucSuffix = selfDomain ? `@muc.${selfDomain}` : null;
  if (mucSuffix && (fromBare.endsWith(mucSuffix) || toBare.endsWith(mucSuffix))) return;

  const isSelf = fromBare === h.selfBareJid;
  const peerJid = isSelf ? toBare : fromBare;
  const nick = localpart(fromBare);

  if (!isSelf && msg.chatState) {
    h.onChatState?.({ peerJid, state: msg.chatState as ChatStateType });
  }

  const retract = ext(msg).retract as { id?: string } | undefined;
  if (retract?.id) {
    h.onMessage?.({
      id: msg.id ?? crypto.randomUUID(),
      peerJid,
      fromJid: fromBare,
      nick,
      body: "",
      createdAt: new Date().toISOString(),
      type: "message",
      retractsId: retract.id,
    });
    return;
  }

  if (msg.marker?.type === "displayed" && msg.marker.id && !isSelf) {
    h.onDisplayed?.({ peerJid, messageId: msg.marker.id });
    return;
  }

  const reactions = ext(msg).reactions as { id?: string; items?: string[] } | undefined;
  if (reactions?.id) {
    h.onReaction?.({ peerJid, messageId: reactions.id, emojis: (reactions.items ?? []).filter((t) => t.length > 0) });
    return;
  }

  if (!msg.body && !msg.subject && !msg.replace) return;

  const liveMsg: LiveDmMessage = {
    id: msg.id ?? crypto.randomUUID(),
    peerJid,
    fromJid: fromBare,
    nick,
    body: msg.body ?? msg.subject ?? "",
    createdAt: new Date().toISOString(),
    type: "message",
  };
  if (msg.replace) {
    liveMsg.replacesId = msg.replace;
  }
  extractMessageExtensions(msg, liveMsg);
  h.onMessage?.(liveMsg);
}
