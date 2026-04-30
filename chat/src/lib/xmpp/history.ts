/** XEP-0313 / XEP-0431: MAM history queries. */
import type { Agent } from "stanza";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveRoomMessage, MamHistoryPage, MamPageParam } from "./types";
import { dispatchGroupchat } from "./message-parsing";

function pagingForPageParam(max: number, pageParam: MamPageParam) {
  return pageParam.type === "latest"
    ? { max, before: "" }
    : { max, before: pageParam.before };
}

function withArchivedMessageId(
  mamResultId: string | undefined,
  innerMsg: ReceivedMessage,
): ReceivedMessage {
  return {
    ...innerMsg,
    id: innerMsg.id ?? mamResultId ?? crypto.randomUUID(),
  } as ReceivedMessage;
}

/**
 * Query message archive for a room.
 *
 * Without `since`: fetches the newest page (`before: ""`) for initial load.
 * With `since`: XEP-0313 §4.1.5 catch-up — pages forward from the cursor via
 * the `start` form field, used after a socket drop to pull anything missed.
 *
 * Throws on IQ/transport errors.
 */
export async function queryMam(
  xmpp: Agent,
  roomJid: string,
  max: number,
  since?: string,
  until?: string,
): Promise<LiveRoomMessage[]> {
  const result = await xmpp.searchHistory(
    roomJid,
    since
      ? {
          paging: { max },
          form: {
            type: "submit",
            fields: [
              { name: "FORM_TYPE", type: "hidden" as const, value: "urn:xmpp:mam:2" },
              { name: "start", value: since },
              ...(until ? [{ name: "end", value: until }] : []),
            ],
          },
        }
      : { paging: { max, before: "" } },
  );
  return parseRoomMamResult(result, roomJid).messages;
}

function parseRoomMamResult(
  result: Awaited<ReturnType<Agent["searchHistory"]>>,
  roomJid: string,
): MamHistoryPage<LiveRoomMessage> {
  const collected: LiveRoomMessage[] = [];

  if (result.results) {
    for (const mamResult of result.results) {
      const innerMsg = mamResult.item?.message;
      if (!innerMsg) continue;

      const timestamp = mamResult.item.delay?.timestamp
        ? mamResult.item.delay.timestamp.toISOString()
        : new Date().toISOString();
      const archivedMsg = withArchivedMessageId(mamResult.id, innerMsg as ReceivedMessage);
      let parsedMessage: LiveRoomMessage | null = null;
      let reactionUpdate: { targetId: string; emojis: string[]; nick: string; senderId: string } | null = null;

      dispatchGroupchat(archivedMsg, {
        currentRoom: roomJid,
        selfNick: "",
        onMessage: (msg) => {
          parsedMessage = { ...msg, createdAt: timestamp };
        },
        onReaction: (event) => {
          reactionUpdate = {
            targetId: event.messageId,
            emojis: event.emojis,
            nick: event.nick,
            senderId: event.authorRealJid ?? `${event.roomJid}/${event.nick}`,
          };
        },
        onChatState: null,
        onDisplayed: null,
        onActivity: null,
      });

      if (parsedMessage) {
        collected.push(parsedMessage);
        continue;
      }

      if (reactionUpdate) {
        const { nick, senderId, targetId, emojis } = reactionUpdate;
        collected.push({
          id: archivedMsg.id ?? crypto.randomUUID(),
          roomJid,
          nick,
          body: "",
          createdAt: timestamp,
          type: "subject",
          _reactionTarget: targetId,
          _reactionEmojis: emojis,
          _reactionSenderId: senderId,
        });
      }
    }
  }
  return {
    messages: collected,
    firstArchiveId: result.paging?.first,
    lastArchiveId: result.paging?.last,
    complete: result.complete === true,
  };
}

export async function queryMamPage(
  xmpp: Agent,
  roomJid: string,
  max: number,
  pageParam: MamPageParam = { type: "latest" },
): Promise<MamHistoryPage<LiveRoomMessage>> {
  const result = await xmpp.searchHistory(roomJid, {
    paging: pagingForPageParam(max, pageParam),
  });
  return parseRoomMamResult(result, roomJid);
}

/**
 * XEP-0313 + XEP-0201: MAM query filtered by thread id. Returns every archived
 * message whose `<thread>` matches the given id, so deep-linked thread panels
 * can backfill messages that the default channel window didn't reach.
 */
export async function queryMamByThread(
  xmpp: Agent,
  roomJid: string,
  threadId: string,
  max: number,
): Promise<LiveRoomMessage[]> {
  if (!threadId) return [];

  const result = await xmpp.searchHistory(roomJid, {
    paging: { max, before: "" },
    form: {
      type: "submit",
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:xmpp:mam:2" },
        { name: "{urn:xmpp:mam:2}thread", value: threadId },
      ],
    },
  });
  return parseRoomMamResult(result, roomJid).messages;
}

export async function queryMamThreadPage(
  xmpp: Agent,
  roomJid: string,
  threadId: string,
  max: number,
  pageParam: MamPageParam = { type: "latest" },
): Promise<MamHistoryPage<LiveRoomMessage>> {
  if (!threadId) return { messages: [], complete: true };

  const result = await xmpp.searchHistory(roomJid, {
    paging: pagingForPageParam(max, pageParam),
    form: {
      type: "submit",
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:xmpp:mam:2" },
        { name: "{urn:xmpp:mam:2}thread", value: threadId },
      ],
    },
  });
  return parseRoomMamResult(result, roomJid);
}

/** XEP-0431: Full-text search in MAM. Throws on IQ/transport errors. */
export async function searchMessages(
  xmpp: Agent,
  roomJid: string,
  query: string,
  max: number,
): Promise<{ id: string; nick: string; body: string; createdAt: string }[]> {
  if (!query.trim()) return [];

  const result = await xmpp.searchHistory(roomJid, {
    paging: { max },
    form: {
      type: "submit",
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:xmpp:mam:2" },
        { name: "fulltext", value: query.trim() },
      ],
    },
  });

  const results: { id: string; nick: string; body: string; createdAt: string }[] = [];
  if (result.results) {
    for (const mamResult of result.results) {
      const innerMsg = mamResult.item?.message;
      if (!innerMsg?.body) continue;

      const from = innerMsg.from ?? "";
      results.push({
        id: innerMsg.id ?? mamResult.id ?? crypto.randomUUID(),
        nick: from.split("/")[1] ?? "unknown",
        body: innerMsg.body,
        createdAt: mamResult.item.delay?.timestamp
          ? mamResult.item.delay.timestamp.toISOString()
          : new Date().toISOString(),
      });
    }
  }
  return results;
}
