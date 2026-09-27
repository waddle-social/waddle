import type { AvatarChangedEvent } from "@/lib/xmpp/client-events";
import { avatarStore, type AvatarStore } from "./avatar-store";
import { occupantJidDirectory, type OccupantJidDirectory } from "./author-jid";

/** The slice of `BrowserXmppClient` the avatar layer drives. */
export interface AvatarClient {
  fetchUserAvatar: (jid: string) => Promise<string | null>;
  addAvatarChangedHandler: (handler: (event: AvatarChangedEvent) => void) => () => void;
  addOccupantRealJidHandler: (handler: (roomJid: string, nick: string, bareJid: string) => void) => () => void;
  addOwnProfilePublishedHandler: (handler: (ownBareJid: string) => void) => () => void;
}

/**
 * Point the avatar store and occupant directory at `client`. Returns the
 * unbind function; cached results survive an unbind so a client swap does
 * not flash initials.
 */
export function bindAvatarClient(
  client: AvatarClient,
  store: AvatarStore = avatarStore,
  directory: OccupantJidDirectory = occupantJidDirectory,
): () => void {
  store.setFetcher((jid) => client.fetchUserAvatar(jid));
  const offChanged = client.addAvatarChangedHandler((event) => store.handleAvatarChanged(event.jid, event.avatarId));
  const offOccupant = client.addOccupantRealJidHandler((roomJid, nick, bareJid) => directory.record(roomJid, nick, bareJid));
  const offOwnProfile = client.addOwnProfilePublishedHandler((ownJid) => store.invalidate(ownJid));
  return () => {
    offChanged();
    offOccupant();
    offOwnProfile();
    store.setFetcher(null);
  };
}

/** Logout: drop every cached face and occupant mapping. */
export function resetAvatars(
  store: AvatarStore = avatarStore,
  directory: OccupantJidDirectory = occupantJidDirectory,
): void {
  store.reset();
  directory.clear();
}
