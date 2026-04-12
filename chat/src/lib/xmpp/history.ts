/** XEP-0313 / XEP-0431: MAM history queries. */
import type { Agent } from "stanza";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveRoomMessage } from "./types";
import { ext, extractMessageExtensions } from "./message-parsing";

/** Query message archive for a room. */
export async function queryMam(
  xmpp: Agent,
  roomJid: string,
  max: number,
): Promise<LiveRoomMessage[]> {
  try {
    const result = await xmpp.searchHistory(roomJid, { paging: { max } });
    const collected: LiveRoomMessage[] = [];

    if (result.results) {
      for (const mamResult of result.results) {
        const innerMsg = mamResult.item?.message;
        if (!innerMsg?.body) continue;

        const from = innerMsg.from ?? "";
        const nick = from.split("/")[1] ?? "unknown";
        const archiveId = mamResult.id ?? crypto.randomUUID();
        const timestamp = mamResult.item.delay?.timestamp
          ? mamResult.item.delay.timestamp.toISOString()
          : new Date().toISOString();

        const msg: LiveRoomMessage = {
          id: archiveId, roomJid, nick, body: innerMsg.body,
          createdAt: timestamp, type: "message",
        };

        // XEP-0444: Reactions in archive
        const reactions = ext(innerMsg).reactions as { id?: string; items?: string[] } | undefined;
        if (reactions?.id) {
          msg.body = "";
          msg.type = "subject";
          (msg as LiveRoomMessage & { _reactionTarget?: string })._reactionTarget = reactions.id;
          (msg as LiveRoomMessage & { _reactionEmojis?: string[] })._reactionEmojis =
            (reactions.items ?? []).filter((t) => t.length > 0);
        }

        extractMessageExtensions(innerMsg as ReceivedMessage, msg);
        collected.push(msg);
      }
    }
    return collected;
  } catch {
    return [];
  }
}

/** XEP-0431: Full-text search in MAM. */
export async function searchMessages(
  xmpp: Agent,
  roomJid: string,
  query: string,
  max: number,
): Promise<{ id: string; nick: string; body: string; createdAt: string }[]> {
  if (!query.trim()) return [];

  try {
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
          id: mamResult.id ?? crypto.randomUUID(),
          nick: from.split("/")[1] ?? "unknown",
          body: innerMsg.body,
          createdAt: mamResult.item.delay?.timestamp
            ? mamResult.item.delay.timestamp.toISOString()
            : new Date().toISOString(),
        });
      }
    }
    return results;
  } catch {
    return [];
  }
}
