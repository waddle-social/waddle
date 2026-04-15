import type { Agent } from "stanza";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveDmMessage } from "./types";
import { barePeerJid } from "./jid";
import { ext, extractMessageExtensions } from "./message-parsing";

function localpart(jid: string): string {
  return barePeerJid(jid).split("@")[0] ?? "unknown";
}

export async function queryPersonalMam(
  xmpp: Agent,
  selfBareJid: string,
  peerBareJid: string,
  max: number,
): Promise<LiveDmMessage[]> {
  const result = await xmpp.searchHistory(selfBareJid, {
    paging: { max },
    form: {
      type: "submit",
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:xmpp:mam:2" },
        { name: "with", value: peerBareJid },
      ],
    },
  });

  const collected: LiveDmMessage[] = [];
  if (!result.results) return collected;

  for (const mamResult of result.results) {
    const innerMsg = mamResult.item?.message;
    if (!innerMsg?.body && !innerMsg?.subject) continue;

    const fromJid = barePeerJid(innerMsg.from ?? "");
    const toJid = barePeerJid(innerMsg.to ?? "");
    const peerJid = fromJid === selfBareJid ? toJid : fromJid;
    if (!peerJid || peerJid !== peerBareJid) continue;

    const archiveId = mamResult.id ?? innerMsg.id ?? crypto.randomUUID();
    const timestamp = mamResult.item.delay?.timestamp
      ? mamResult.item.delay.timestamp.toISOString()
      : new Date().toISOString();

    const msg: LiveDmMessage = {
      id: archiveId,
      peerJid,
      fromJid,
      nick: localpart(fromJid),
      body: innerMsg.body ?? innerMsg.subject ?? "",
      createdAt: timestamp,
      type: "message",
    };

    const reactions = ext(innerMsg).reactions as { id?: string; items?: string[] } | undefined;
    if (reactions?.id) {
      msg.body = "";
      msg._reactionTarget = reactions.id;
      msg._reactionEmojis = (reactions.items ?? []).filter((t) => t.length > 0);
    }

    extractMessageExtensions(innerMsg as ReceivedMessage, msg);
    collected.push(msg);
  }

  return collected;
}

export async function searchDmMessages(
  xmpp: Agent,
  selfBareJid: string,
  peerBareJid: string,
  query: string,
  max: number,
): Promise<{ id: string; nick: string; body: string; createdAt: string }[]> {
  if (!query.trim()) return [];
  const result = await xmpp.searchHistory(selfBareJid, {
    paging: { max },
    form: {
      type: "submit",
      fields: [
        { name: "FORM_TYPE", type: "hidden", value: "urn:xmpp:mam:2" },
        { name: "with", value: peerBareJid },
        { name: "fulltext", value: query.trim() },
      ],
    },
  });
  const results: { id: string; nick: string; body: string; createdAt: string }[] = [];
  if (!result.results) return results;
  for (const mamResult of result.results) {
    const innerMsg = mamResult.item?.message;
    if (!innerMsg?.body) continue;
    const fromJid = barePeerJid(innerMsg.from ?? "");
    if (!fromJid) continue;
    results.push({
      id: mamResult.id ?? innerMsg.id ?? crypto.randomUUID(),
      nick: localpart(fromJid),
      body: innerMsg.body,
      createdAt: mamResult.item.delay?.timestamp
        ? mamResult.item.delay.timestamp.toISOString()
        : new Date().toISOString(),
    });
  }
  return results;
}
