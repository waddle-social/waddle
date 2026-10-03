import type { AvatarChangedEvent } from "@/lib/xmpp/client-events";
import { avatarStore, type AvatarStore } from "./avatar-store";
import { occupantJidDirectory, type OccupantJidDirectory } from "./author-jid";
import { extensionServiceJidForUserJid } from "@/lib/xmpp/extension-commands";

/** The slice of `BrowserXmppClient` the avatar layer drives. */
export interface AvatarClient {
  fetchUserAvatar: (jid: string) => Promise<string | null>;
  forgetUserAvatar: (jid: string) => void;
  addAvatarChangedHandler: (handler: (event: AvatarChangedEvent) => void) => () => void;
  addOccupantRealJidHandler: (handler: (roomJid: string, nick: string, bareJid: string | null, isBot?: boolean) => void) => () => void;
  addOwnProfilePublishedHandler: (handler: (ownBareJid: string) => void) => () => void;
  addOwnOccupantNickHandler: (handler: (roomJid: string, nick: string | null) => void) => () => void;
  readonly bareJid: string;
}

/**
 * Point the avatar store and occupant directory at `client`. Returns the
 * unbind function; cached results survive an unbind so a client swap does
 * not flash initials.
 */
function bindAvatarClient(
  client: AvatarClient,
  store: AvatarStore = avatarStore,
  directory: OccupantJidDirectory = occupantJidDirectory,
): () => void {
  directory.setExtensionsDomain(extensionServiceJidForUserJid(client.bareJid));
  store.setFetcher((jid) => client.fetchUserAvatar(jid));
  store.setEvictionHandler((jid) => client.forgetUserAvatar(jid));
  const offChanged = client.addAvatarChangedHandler((event) => store.handleAvatarChanged(event.jid, event.avatarId));
  const offOccupant = client.addOccupantRealJidHandler((roomJid, nick, bareJid, isBot) => directory.record(roomJid, nick, bareJid, isBot));
  const offOwnProfile = client.addOwnProfilePublishedHandler((ownJid) => store.invalidate(ownJid));
  const offOwnNick = client.addOwnOccupantNickHandler((roomJid, nick) => directory.recordOwnNick(roomJid, nick));
  return () => {
    offChanged();
    offOccupant();
    offOwnProfile();
    offOwnNick();
    store.setFetcher(null);
    store.setEvictionHandler(null);
  };
}

/**
 * The shell's single avatar binding: at most one client is bound at a
 * time, and logout unbinds that client BEFORE forgetting everything, so a
 * late event from the old account cannot repopulate the next one's store.
 */
export function createAvatarBinding(
  store: AvatarStore = avatarStore,
  directory: OccupantJidDirectory = occupantJidDirectory,
) {
  let unbind: (() => void) | null = null;
  return {
    bind(client: AvatarClient): void {
      unbind?.();
      unbind = bindAvatarClient(client, store, directory);
    },
    unbind(): void {
      unbind?.();
      unbind = null;
    },
    logout(): void {
      unbind?.();
      unbind = null;
      store.reset();
      directory.clear();
    },
  };
}
