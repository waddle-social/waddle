/** XEP-0313 / XEP-0431: MAM history queries. */
import type { Agent } from "stanza";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveRoomMessage, MamHistoryPage, MamPageParam, MessageSearchResult } from "./types";
import { dispatchGroupchat } from "./message-parsing";

const WADDLE_MAM_THREAD_FIELD = "{urn:waddle:mam-thread:0}thread";
const FULLTEXT_MAM_FIELD = "{urn:xmpp:fulltext:0}fulltext";

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
      let parsedMessage: LiveRoomMessage | undefined;
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
        // XEP-0461 §3: a reply identifies the replied-to message; it does
        // not imply XEP-0201 thread membership. Threaded messages must
        // carry their own <thread/>; the server archives <thread parent=...>
        // faithfully, so anything that should reload as part of a thread
        // already has the metadata. Do not synthesize a threadId from
        // <reply id=...> here.
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
        { name: WADDLE_MAM_THREAD_FIELD, value: threadId },
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
        { name: WADDLE_MAM_THREAD_FIELD, value: threadId },
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
): Promise<MessageSearchResult[]> {
  if (!query.trim()) return [];

  const result = await xmpp.searchHistory(roomJid, {
    paging: { max },
    form: {
      type: "submit",
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:xmpp:mam:2" },
        { name: FULLTEXT_MAM_FIELD, value: query.trim() },
      ],
    },
  });

  const parsed = parseRoomMamResult(result, roomJid).messages;
  const archiveIds = result.results?.map((mamResult) => mamResult.id).filter(Boolean) ?? [];
  const results: MessageSearchResult[] = [];
  for (const [index, message] of parsed.entries()) {
    if (!message.body) continue;
    results.push({
      id: message.id,
      ...(archiveIds[index] ? { archiveId: archiveIds[index] } : {}),
      nick: message.nick,
      body: message.body,
      createdAt: message.createdAt,
      ...(message.threadId ? { threadId: message.threadId } : {}),
      ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}),
      roomJid: message.roomJid,
    });
  }
  return results;
}
