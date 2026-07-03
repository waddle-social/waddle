/**
 * Presence handling extracted from `BrowserXmppClient` (stage-2
 * decomposition of `client.ts`): outbound presence (RFC 6121 §4.7 show
 * + XEP-0319 idle), peer presence subscription, and the inbound
 * presence parse/wiring — MUC occupant hats (XEP-0317), authority
 * (affiliation/role), per-room presence caches, member-JID discovery,
 * and 1:1 presence updates. MUC *join* state stays with the client's
 * join machinery; this module reports self-presence transitions
 * through narrow callbacks.
 */
import { barePeerJid, fullJidIdentityKey, resourceOf } from "./jid";
import type { ClientEvents, TypedEventBus } from "./client-events";
import { parseMucAffiliation, parseMucRole } from "./types";
import type { RoomAuthority, RoomHats, RoomPresence } from "./types";
import {
  mapPresenceIdleSince,
  mapPresenceShow,
  parsePresenceShow,
} from "./wasm-message-codecs";
import type { WasmPresence } from "./wasm-types";
import type { BroadcastShow } from "@/presence/effective-show";

/** Structural subset of the WASM client the presence module drives. */
type PresenceWasmClient = {
  send_presence?: (status?: string | null, show?: string | null, idleSince?: string | null) => Promise<unknown>;
  subscribe_to_presence?: (peerJid: string) => Promise<unknown>;
};

/**
 * True when a presence carries an `http://jabber.org/protocol/muc#user`
 * payload. Every MUC occupant presence includes an affiliation/role
 * (and self-presence a status code), so we no longer rely on a
 * disclosed real JID — anonymous rooms omit `muc_jid` yet must still
 * be routed through MUC handling.
 */
function isMucPresence(presence: WasmPresence): boolean {
  return (
    presence.muc_jid !== undefined ||
    presence.muc_affiliation !== undefined ||
    presence.muc_role !== undefined ||
    (presence.muc_status_codes?.length ?? 0) > 0
  );
}

type PresenceManagerDeps = {
  events: TypedEventBus<ClientEvents>;
  /** Focused room — only its presence fan-out reaches the room handlers. */
  currentRoom: () => string | null;
  /** Identity keys of our own full JID (session and resource variants). */
  ownFullJidCandidates: () => Set<string>;
  requireConnectedXmpp: () => Promise<PresenceWasmClient>;
  /** MUC error presence addressed to a pending join; true when consumed. */
  handleMucPresenceError: (presence: WasmPresence) => boolean;
  /** Own available self-presence (XEP-0045 §7.2.2 status 110): the join completed. */
  onOwnSelfPresence: (roomJid: string) => void;
  /** Own unavailable self-presence that is not a §7.6 nick change: readiness revoked. */
  onOwnUnavailable: (roomJid: string) => void;
};

export class PresenceManager {
  private roomHatsByRoom = new Map<string, RoomHats>();
  private roomAuthorityByRoom = new Map<string, RoomAuthority>();
  private roomPresenceByRoom = new Map<string, RoomPresence>();
  private roomMemberJidsByRoom = new Map<string, Record<string, string>>();

  constructor(private readonly deps: PresenceManagerDeps) {}

  /** Publish the user's own Show plus an optional XEP-0319 idle stamp.
   * RFC 6121 §4.7.2.1: Available is the absence of a `<show>`, so it sends no
   * show element. `idleSince` (epoch ms) is serialised to an xs:dateTime; a
   * null/omitted value sends no `<idle/>` (XEP-0319 return-from-idle). */
  async setPresence(show: BroadcastShow, idleSince?: number | null): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.send_presence?.(undefined, show === "available" ? undefined : show, idleSince != null ? new Date(idleSince).toISOString() : undefined);
  }

  async subscribeToPeerPresence(peerJid: string): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.subscribe_to_presence?.(barePeerJid(peerJid));
  }

  private isOwnMucSelfPresence(presence: {
    muc_jid?: string | null;
    muc_status_codes?: number[] | null;
  }): boolean {
    // XEP-0045 §7.2.2: status code 110 is the canonical self-presence
    // marker. It is the only signal that works regardless of room
    // anonymity (no real JID disclosed) or a service-modified nick.
    if (presence.muc_status_codes?.includes(110)) return true;
    // Fallback for any path that doesn't surface status codes: match
    // the disclosed real JID (non-anonymous rooms only).
    const mucJid = fullJidIdentityKey(presence.muc_jid);
    return !!mucJid && this.deps.ownFullJidCandidates().has(mucJid);
  }

  private isOwnAvailableMucSelfPresence(presence: {
    muc_jid?: string | null;
    muc_status_codes?: number[] | null;
    presence_type?: string;
  }): boolean {
    return this.isOwnMucSelfPresence(presence) && presence.presence_type !== "unavailable";
  }

  private hatsFor(roomJid: string): RoomHats {
    let cached = this.roomHatsByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomHatsByRoom.set(roomJid, cached);
    }
    return cached;
  }

  private authorityFor(roomJid: string): RoomAuthority {
    let cached = this.roomAuthorityByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomAuthorityByRoom.set(roomJid, cached);
    }
    return cached;
  }

  private presenceFor(roomJid: string): RoomPresence {
    let cached = this.roomPresenceByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomPresenceByRoom.set(roomJid, cached);
    }
    return cached;
  }

  memberJidsFor(roomJid: string): Record<string, string> {
    let cached = this.roomMemberJidsByRoom.get(roomJid);
    if (!cached) {
      cached = {};
      this.roomMemberJidsByRoom.set(roomJid, cached);
    }
    return cached;
  }

  clearAll(): void {
    this.roomHatsByRoom.clear();
    this.roomAuthorityByRoom.clear();
    this.roomPresenceByRoom.clear();
    this.roomMemberJidsByRoom.clear();
  }

  /** Replay the focused room's cached presence state to the room handlers. */
  dispatchFocusedRoom(): void {
    const roomJid = this.deps.currentRoom();
    if (!roomJid) {
      this.deps.events.emit("hats", {});
      this.deps.events.emit("authority", {});
      this.deps.events.emit("presence", {});
      return;
    }
    this.deps.events.emit("hats", { ...(this.roomHatsByRoom.get(roomJid) ?? {}) });
    this.deps.events.emit("authority", { ...(this.roomAuthorityByRoom.get(roomJid) ?? {}) });
    this.deps.events.emit("presence", { ...(this.roomPresenceByRoom.get(roomJid) ?? {}) });
    const memberJids = this.roomMemberJidsByRoom.get(roomJid) ?? {};
    for (const [nick, bare] of Object.entries(memberJids)) {
      this.deps.events.emit("memberJid", nick, bare);
    }
  }

  handle(presence: WasmPresence): void {
    const from = presence.from ?? "";
    if (this.deps.handleMucPresenceError(presence)) return;
    if (isMucPresence(presence)) {
      const [room, nick = ""] = from.split("/");
      if (!room) return;
      // XEP-0045 §7.2.2: our own available self-presence (status 110,
      // or, as a fallback, our disclosed real JID) completes the join.
      // Resolve by room — not the exact occupant JID — so a
      // service-modified nick or localpart-case difference still
      // matches the waiter.
      if (this.isOwnAvailableMucSelfPresence(presence)) {
        this.deps.onOwnSelfPresence(room);
      }
      if (!nick && presence.vcard_avatar) this.deps.events.emit("roomAvatar", room, presence.vcard_avatar);
      if (!nick) return;
      const roomHats = this.hatsFor(room);
      const roomAuthority = this.authorityFor(room);
      const roomPresence = this.presenceFor(room);
      const roomMemberJids = this.memberJidsFor(room);
      const isFocusedRoom = room === this.deps.currentRoom();
      if (presence.presence_type === "unavailable") {
        // XEP-0045 §7.6: a nick change sends an unavailable presence for
        // the old nick carrying our own status 110 *and* 303. That is not
        // a departure — the available presence for the new nick follows —
        // so it must NOT revoke room readiness.
        const isNickChange = presence.muc_status_codes?.includes(303) ?? false;
        if (this.isOwnMucSelfPresence(presence) && !isNickChange) {
          this.deps.onOwnUnavailable(room);
        }
        delete roomHats[nick];
        delete roomAuthority[nick];
        roomPresence[nick] = "offline";
        delete roomMemberJids[nick];
        if (isFocusedRoom) {
          this.deps.events.emit("hats", { ...roomHats });
          this.deps.events.emit("authority", { ...roomAuthority });
          this.deps.events.emit("presence", { ...roomPresence });
          this.deps.events.emit("lastSeen", nick, Date.now());
        }
        return;
      }
      // XEP-0317 hats are server-emitted descriptive metadata only.
      // No client-side fabrication from muc_affiliation/muc_role —
      // those flow as `roomAuthority` and drive authority chips
      // independently (see `parseMucAffiliation` / `parseMucRole`).
      roomHats[nick] = (presence.hats ?? []).map((hat) => ({
        uri: hat.uri,
        title: hat.title,
      }));
      roomAuthority[nick] = {
        affiliation: parseMucAffiliation(presence.muc_affiliation),
        role: parseMucRole(presence.muc_role),
      };
      roomPresence[nick] = parsePresenceShow(presence.show);
      if (presence.muc_jid) {
        const bare = barePeerJid(presence.muc_jid);
        roomMemberJids[nick] = bare;
        if (isFocusedRoom) this.deps.events.emit("memberJid", nick, bare);
      }
      if (isFocusedRoom) {
        this.deps.events.emit("hats", { ...roomHats });
        this.deps.events.emit("authority", { ...roomAuthority });
        this.deps.events.emit("presence", { ...roomPresence });
      }
      return;
    }
    const bare = barePeerJid(from);
    if (!bare) return;
    if (presence.presence_type === "subscribe") return;
    const idleSince = mapPresenceIdleSince(presence);
    this.deps.events.emit("presenceUpdate", {
      bareJid: bare,
      resource: resourceOf(from),
      show: mapPresenceShow(presence),
      ...(presence.status ? { status: presence.status } : {}),
      ...(idleSince !== undefined ? { idleSince } : {}),
    });
  }
}
