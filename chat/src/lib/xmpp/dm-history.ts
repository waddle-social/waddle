import type { Agent } from "stanza";
import type { ReceivedMessage } from "stanza/protocol";
import type { LiveDmMessage, MamHistoryPage, MamPageParam } from "./types";
import { barePeerJid } from "./jid";
import { dispatchChat } from "./dm-parsing";

function localpart(jid: string): string {
  return barePeerJid(jid).split("@")[0] ?? "unknown";
}

function isItemNotFound(err: unknown): boolean {
  if (err && typeof err === "object") {
    const e = err as Record<string, unknown>;
    if (e.condition === "item-not-found") return true;
    if (typeof e.error === "object" && e.error !== null) {
      const inner = e.error as Record<string, unknown>;
      if (inner.condition === "item-not-found") return true;
    }
  }
  return false;
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

function pagingForPageParam(max: number, pageParam: MamPageParam) {
  return pageParam.type === "latest"
    ? { max, before: "" }
    : { max, before: pageParam.before };
}

export async function queryPersonalMam(
  xmpp: Agent,
  selfBareJid: string,
  peerBareJid: string,
  max: number,
  since?: string,
  until?: string,
): Promise<LiveDmMessage[]> {
  // XEP-0313 §4.1.5: when fetching the most recent page (no `since`), we use
  // `paging.before: ""` to page backwards from the end of the archive.
  // For catch-up (`since` set), we instead page forward from the cursor by
  // adding a `start` form field and dropping `before`.
  const baseFields = [
    { name: "FORM_TYPE", type: "hidden" as const, value: "urn:xmpp:mam:2" },
    { name: "with", value: peerBareJid },
  ];
  const fields = since
    ? [
        ...baseFields,
        { name: "start", value: since },
        ...(until ? [{ name: "end", value: until }] : []),
      ]
    : baseFields;
  const paging = since ? { max } : { max, before: "" };

  let result;
  try {
    result = await xmpp.searchHistory(selfBareJid, {
      paging,
      form: { type: "submit", fields },
    });
  } catch (err) {
    if (isItemNotFound(err)) return [];
    throw err;
  }

  return parseDmMamResult(result, selfBareJid, peerBareJid).messages;
}

function parseDmMamResult(
  result: Awaited<ReturnType<Agent["searchHistory"]>>,
  selfBareJid: string,
  peerBareJid: string,
): MamHistoryPage<LiveDmMessage> {
  const collected: LiveDmMessage[] = [];
  if (!result.results) {
    return {
      messages: collected,
      firstArchiveId: result.paging?.first,
      lastArchiveId: result.paging?.last,
      complete: result.complete === true,
    };
  }

  for (const mamResult of result.results) {
    const innerMsg = mamResult.item?.message;
    if (!innerMsg) continue;

    const archivedMsg = withArchivedMessageId(mamResult.id, innerMsg as ReceivedMessage);
    const fromJid = barePeerJid(archivedMsg.from ?? "");
    const toJid = barePeerJid(archivedMsg.to ?? "");
    const peerJid = fromJid === selfBareJid ? toJid : fromJid;
    if (!peerJid || peerJid !== peerBareJid) continue;

    const timestamp = mamResult.item.delay?.timestamp
      ? mamResult.item.delay.timestamp.toISOString()
      : new Date().toISOString();
    let parsedMessage: LiveDmMessage | null = null;
    let reactionUpdate: { targetId: string; emojis: string[] } | null = null;

    dispatchChat(archivedMsg, {
      selfBareJid,
      onMessage: (msg) => {
        parsedMessage = { ...msg, createdAt: timestamp };
      },
      onReaction: (event) => {
        reactionUpdate = {
          targetId: event.messageId,
          emojis: event.emojis,
        };
      },
      onChatState: null,
      onDisplayed: null,
    });

    if (parsedMessage) {
      collected.push(parsedMessage);
      continue;
    }

    if (reactionUpdate) {
      const { targetId, emojis } = reactionUpdate;
      collected.push({
        id: archivedMsg.id ?? crypto.randomUUID(),
        peerJid,
        fromJid,
        nick: localpart(fromJid),
        body: "",
        createdAt: timestamp,
        type: "message",
        _reactionTarget: targetId,
        _reactionEmojis: emojis,
      });
    }
  }

  return {
    messages: collected,
    firstArchiveId: result.paging?.first,
    lastArchiveId: result.paging?.last,
    complete: result.complete === true,
  };
}

export async function queryPersonalMamPage(
  xmpp: Agent,
  selfBareJid: string,
  peerBareJid: string,
  max: number,
  pageParam: MamPageParam = { type: "latest" },
): Promise<MamHistoryPage<LiveDmMessage>> {
  const fields = [
    { name: "FORM_TYPE", type: "hidden" as const, value: "urn:xmpp:mam:2" },
    { name: "with", value: peerBareJid },
  ];

  let result;
  try {
    result = await xmpp.searchHistory(selfBareJid, {
      paging: pagingForPageParam(max, pageParam),
      form: { type: "submit", fields },
    });
  } catch (err) {
    if (isItemNotFound(err)) return { messages: [], complete: true };
    throw err;
  }

  return parseDmMamResult(result, selfBareJid, peerBareJid);
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
      id: innerMsg.id ?? mamResult.id ?? crypto.randomUUID(),
      nick: localpart(fromJid),
      body: innerMsg.body,
      createdAt: mamResult.item.delay?.timestamp
        ? mamResult.item.delay.timestamp.toISOString()
        : new Date().toISOString(),
    });
  }
  return results;
}
