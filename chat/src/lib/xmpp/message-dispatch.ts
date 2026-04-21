import type { ReceivedMessage } from "stanza/protocol";
import { dispatchChat, type DmHandlers } from "./dm-parsing";
import { dispatchGroupchat, type GroupchatHandlers } from "./message-parsing";

/**
 * Returns a handler for the stanza library's generic `message` event that
 * routes groupchat + 1:1 messages to the right parser.
 *
 * We intentionally listen on `message` rather than on the more specific
 * `groupchat` / `chat` events because stanzajs only emits those when the
 * message carries a body or embedded link (see `stanza/Client.js`'s
 * `isChat` gate). Body-less payloads — reactions, chat states, delivery
 * markers — never fire `groupchat`/`chat`, so anything registered there
 * would silently miss them.
 *
 * Carbon-wrapped messages are skipped here; `carbon:sent` /
 * `carbon:received` listeners unwrap and forward them separately.
 */
export function buildMessageDispatcher(
  groupchatHandlers: (msg?: ReceivedMessage) => GroupchatHandlers,
  chatHandlers: (msg?: ReceivedMessage) => DmHandlers,
): (msg: ReceivedMessage) => void {
  return (msg) => {
    if (msg.type === "error") return;
    if ((msg as { carbon?: unknown }).carbon) return;

    if (msg.type === "groupchat") {
      dispatchGroupchat(msg, groupchatHandlers(msg));
      return;
    }

    if (msg.type === "chat" || msg.type === "normal" || !msg.type) {
      dispatchChat(msg, chatHandlers(msg));
    }
  };
}
