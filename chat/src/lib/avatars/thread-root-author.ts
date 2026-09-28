import type { TimelineMessage } from "@/lib/chat-ui";
import { barePeerJid } from "@/lib/xmpp/jid";
import { authorAvatarJid } from "./author-jid";

/** What the threads list needs to find a thread's starter among loaded rows. */
export interface ThreadRootLookup {
  /** Bare room JID of the channel whose timeline is loaded, if any. */
  loadedRoomJid: string | null;
  /** The loaded timeline's thread index: root row by thread id. */
  resolveRoot: (threadId: string) => TimelineMessage | null | undefined;
  selfJid?: string | null;
}

/**
 * Avatar JID for a threads-list row's starter. The threads query names the
 * starter only by room nick, and a nick may have changed hands since, so
 * the avatar comes only from the loaded root row's own resolved author
 * (archive real JID or ingest stamp). Anything else renders initials.
 */
export function threadRootAvatarJid(
  entry: { channel: string; thread_id: string },
  lookup: ThreadRootLookup,
): string | null {
  const loaded = lookup.loadedRoomJid;
  if (!loaded || barePeerJid(entry.channel).toLowerCase() !== barePeerJid(loaded).toLowerCase()) return null;
  const root = lookup.resolveRoot(entry.thread_id);
  return root ? authorAvatarJid(root, lookup.selfJid) : null;
}
