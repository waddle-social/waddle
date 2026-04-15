/** Inbound message parsing — extracts XEP extension data from stanza messages. */
import type { ReceivedMessage } from "stanza/protocol";
import type {
  CallInviteInfo, ChatStateEvent, ChatStateType, DisplayedEvent,
  LiveDmMessage, LiveRoomMessage, ReactionEvent, RoomActivityEvent, SharedFileInfo,
} from "./types";

type MessageExtensionsTarget = Pick<
  LiveRoomMessage,
  "body" | "mentions" | "broadcastMention" | "callInvite" | "sharedFile" | "isSticker" | "replacesId"
>;

/** Access custom JXT extension fields that TypeScript doesn't know about. */
export function ext(msg: unknown): Record<string, unknown> {
  return msg as Record<string, unknown>;
}

/** Populate a LiveRoomMessage with data from XEP extensions on the stanza. */
export function extractMessageExtensions(
  msg: ReceivedMessage,
  base: LiveRoomMessage | LiveDmMessage,
): void {
  if (msg.replace) {
    base.replacesId = msg.replace;
  }

  extractReferences(msg, base);
  extractExplicitMentions(msg, base);
  extractCallInvite(msg, base);
  extractFileSharing(msg, base);
  extractMarkup(msg, base);

  if (ext(msg).sticker) {
    base.isSticker = true;
  }
}

function extractReferences(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const refs = ext(msg).references as Array<{ type?: string; uri?: string }> | undefined;
  if (!refs?.length) return;

  const mentionUris = refs
    .filter((r) => r.type === "mention" && r.uri)
    .map((r) => (r.uri as string).replace(/^xmpp:/, ""));
  if (mentionUris.length > 0) {
    base.mentions = mentionUris;
  }
}

function extractExplicitMentions(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const em = ext(msg).explicitMentions as { items?: Array<{ type?: string }> } | undefined;
  if (!em?.items) return;

  for (const m of em.items) {
    if (m.type === "everyone") { base.broadcastMention = "everyone"; return; }
    if (m.type === "here") { base.broadcastMention = "here"; return; }
  }
}

function extractCallInvite(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const inviteElement = ext(msg).callInvite as
    | { id?: string; muji?: boolean; jingleSid?: string; jingleJid?: string; externalUri?: string }
    | undefined;
  if (!inviteElement) return;

  const invite: CallInviteInfo = {
    inviteId: inviteElement.id ?? msg.id ?? crypto.randomUUID(),
    muji: inviteElement.muji ?? false,
  };

  if (inviteElement.jingleSid) invite.jingleSid = inviteElement.jingleSid;
  if (inviteElement.jingleJid) invite.jingleJid = inviteElement.jingleJid;

  const meeting = ext(msg).meeting as { url?: string; desc?: string } | undefined;
  const resolvedUri = inviteElement.externalUri ?? meeting?.url;
  if (resolvedUri) invite.externalUri = resolvedUri;
  if (meeting?.desc) invite.meetingDesc = meeting.desc;
  base.callInvite = invite;
}

/** Callbacks the groupchat dispatcher invokes on the client. */
export interface GroupchatHandlers {
  currentRoom: string | null;
  selfNick: string;
  onMessage: ((msg: LiveRoomMessage) => void) | null;
  onChatState: ((event: ChatStateEvent) => void) | null;
  onDisplayed: ((event: DisplayedEvent) => void) | null;
  onReaction: ((event: ReactionEvent) => void) | null;
  onActivity: ((event: RoomActivityEvent) => void) | null;
}

/** Route an inbound groupchat message to the appropriate handler. */
export function dispatchGroupchat(msg: ReceivedMessage, h: GroupchatHandlers): void {
  const from = msg.from ?? "";
  const [roomJid, nick = "unknown"] = from.split("/");
  if (!roomJid) return;

  if (roomJid !== h.currentRoom) {
    if (msg.body) {
      const partial: LiveRoomMessage = { id: "", roomJid, nick, body: msg.body, createdAt: "", type: "message" };
      extractReferences(msg, partial);
      extractExplicitMentions(msg, partial);
      const activity: RoomActivityEvent = { roomJid, nick, body: msg.body };
      if (partial.mentions) activity.mentions = partial.mentions;
      if (partial.broadcastMention) activity.broadcastMention = partial.broadcastMention;
      h.onActivity?.(activity);
    }
    return;
  }

  if (nick !== h.selfNick && msg.chatState) {
    h.onChatState?.({ roomJid, nick, state: msg.chatState as ChatStateType });
  }

  const applyTo = ext(msg).applyTo as { id?: string; moderated?: { retract?: boolean } } | undefined;
  if (applyTo?.id && applyTo.moderated) {
    h.onMessage?.({ id: msg.id ?? crypto.randomUUID(), roomJid, nick, body: "", createdAt: new Date().toISOString(), type: "message", retractsId: applyTo.id });
    return;
  }

  const retract = ext(msg).retract as { id?: string } | undefined;
  if (retract?.id) {
    h.onMessage?.({ id: msg.id ?? crypto.randomUUID(), roomJid, nick, body: "", createdAt: new Date().toISOString(), type: "message", retractsId: retract.id });
    return;
  }

  if (msg.marker?.type === "displayed" && msg.marker.id && nick !== h.selfNick) {
    h.onDisplayed?.({ roomJid, nick, messageId: msg.marker.id });
    return;
  }

  const reactions = ext(msg).reactions as { id?: string; items?: string[] } | undefined;
  if (reactions?.id) {
    h.onReaction?.({ roomJid, nick, messageId: reactions.id, emojis: (reactions.items ?? []).filter((t) => t.length > 0) });
    return;
  }

  if (!msg.body && !msg.subject) return;

  const liveMsg: LiveRoomMessage = {
    id: msg.id ?? crypto.randomUUID(), roomJid, nick,
    body: msg.body ?? msg.subject ?? "",
    createdAt: new Date().toISOString(),
    type: msg.body ? "message" : "subject",
  };
  extractMessageExtensions(msg, liveMsg);
  h.onMessage?.(liveMsg);
}

function extractFileSharing(msg: ReceivedMessage, base: MessageExtensionsTarget): void {
  const fs = ext(msg).fileSharing as
    | { disposition?: string; name?: string; mediaType?: string; size?: string; width?: string; height?: string; desc?: string; url?: string }
    | undefined;
  if (!fs?.url) return;

  const info: SharedFileInfo = {
    url: fs.url,
    disposition: fs.disposition === "attachment" ? "attachment" : "inline",
  };
  if (fs.name) info.name = fs.name;
  if (fs.mediaType) info.mediaType = fs.mediaType;
  if (fs.size) info.size = parseInt(fs.size, 10);
  if (fs.width) info.width = parseInt(fs.width, 10);
  if (fs.height) info.height = parseInt(fs.height, 10);
  if (fs.desc) info.desc = fs.desc;
  base.sharedFile = info;
}

/** XEP-0394: Extract Message Markup annotations. */
function extractMarkup(msg: ReceivedMessage, base: LiveRoomMessage): void {
  const markupData = ext(msg).markup as { spans?: Array<{ type: string; start: number; end: number; uri?: string }> } | undefined;
  if (!markupData?.spans || markupData.spans.length === 0) return;

  base.markup = markupData.spans.map(s => ({
    type: s.type as import("@/lib/chat-ui").MarkupSpan["type"],
    start: s.start,
    end: s.end,
    ...(s.uri ? { uri: s.uri } : {}),
  }));
}
