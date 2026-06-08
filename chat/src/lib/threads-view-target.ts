// Classify a global Threads-view row (`urn:waddle:threads:0`) by the bare
// JID it carries so the shell can route a click to the right surface.
//
// The threads list mixes channel (MUC room) threads with DM
// (partner-keyed) threads. Channel rows carry a managed room JID on the
// MUC service domain; DM rows carry the partner's bare JID on a user
// domain. The only durable discriminator is the domain — see
// `isTrustedManagedRoomJid` — with an explicit channel-JID match taking
// precedence so a known channel on a foreign conference domain still
// resolves as a channel.

import type { ChannelSummary } from "@/lib/chat-types";
import { isTrustedManagedRoomJid } from "@/lib/channel-room";
import { resolveChannelIdForRoomJid } from "@/lib/threads-channel-resolve";
import { barePeerJid } from "@/lib/xmpp/jid";

export type ThreadEntryTarget =
  | { kind: "channel"; channelId: string }
  | { kind: "dm"; peerJid: string };

interface ResolveThreadEntryTargetOptions {
  channels: readonly Pick<ChannelSummary, "id" | "jid">[];
  managedMucDomain: string;
}

/**
 * Returns the routing target for a Threads-view entry JID, or `null` when
 * the JID is unusable (empty / no local-part).
 *
 * Resolution order:
 *  1. Explicit channel match by exact room JID.
 *  2. Managed MUC room (domain matches `managedMucDomain`) → channel,
 *     resolving the channel-id slug via the existing room-JID resolver.
 *  3. Otherwise a DM partner JID → open the direct-message conversation.
 */
export function resolveThreadEntryTarget(
  channelJid: string,
  { channels, managedMucDomain }: ResolveThreadEntryTargetOptions,
): ThreadEntryTarget | null {
  const bare = barePeerJid(channelJid);
  if (!bare || !bare.includes("@") || bare.startsWith("@")) return null;

  const explicit = channels.find(
    (channel) => channel.jid && barePeerJid(channel.jid) === bare,
  );
  if (explicit) return { kind: "channel", channelId: explicit.id };

  if (isTrustedManagedRoomJid(bare, managedMucDomain)) {
    const channelId = resolveChannelIdForRoomJid(bare, channels);
    return channelId ? { kind: "channel", channelId } : null;
  }

  return { kind: "dm", peerJid: bare };
}
