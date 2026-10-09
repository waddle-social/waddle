import type { TimelineMessage } from "@/lib/chat-ui";
import { bareJidKey } from "@/lib/xmpp-client";

/** Resolve room references without selecting the first of retained ID twins. */
export function resolveRoomMessageTarget(
  messages: readonly TimelineMessage[],
  targetId: string,
  { preferCanonical = true }: { preferCanonical?: boolean } = {},
): { message?: TimelineMessage; ambiguous: boolean } {
  if (preferCanonical) {
    const canonical = messages.filter((message) =>
      message.stanzaId === targetId
      && !!message.stanzaIdBy && !!message.authorOccupantJid
      && bareJidKey(message.stanzaIdBy) === bareJidKey(message.authorOccupantJid)
    );
    if (canonical.length > 0) return canonical.length === 1
      ? { message: canonical[0], ambiguous: false }
      : { ambiguous: true };
  }
  const claimants = messages.filter((message) =>
    message.id === targetId || message.wireIds?.includes(targetId)
  );
  return claimants.length === 1
    ? { message: claimants[0], ambiguous: false }
    : { ambiguous: claimants.length > 1 };
}
