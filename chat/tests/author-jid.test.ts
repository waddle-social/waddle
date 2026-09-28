import { afterEach, describe, expect, test } from "bun:test";
import { computed } from "vue";
import {
  OccupantJidDirectory,
  authorAvatarJid,
  occupantJidDirectory,
  resolveAuthorJid,
  stampLiveRoomAuthor,
  typingAuthorAvatarJid,
} from "../src/lib/avatars/author-jid";
import { AvatarStore } from "../src/lib/avatars/avatar-store";

const ROOM = "general@muc.waddle.social";
const SELF = "me@waddle.social/web";

afterEach(() => occupantJidDirectory.clear());

/** A live room row by `nick`, as the live path maps it (before stamping). */
function liveRow(nick: string, extra: Record<string, unknown> = {}) {
  return { authorJid: `${ROOM}/${nick}`, authorOccupantJid: `${ROOM}/${nick}`, createdAtSource: "fallback" as const, ...extra };
}

describe("resolveAuthorJid: identity the row carries, else initials", () => {
  test("the MUC archive real JID wins", () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "alice", "someone-else@waddle.social");
    expect(resolveAuthorJid({
      authorOccupantJid: `${ROOM}/alice`,
      authorRealJid: "Alice@Waddle.social/phone",
      createdAtSource: "archive",
    }, directory)).toBe("alice@waddle.social");
  });

  test("the ingest stamp is used when there is no real JID", () => {
    const directory = new OccupantJidDirectory();
    expect(resolveAuthorJid({ authorOccupantJid: `${ROOM}/alice`, authorAvatarJid: "alice@waddle.social" }, directory))
      .toBe("alice@waddle.social");
  });

  test("an unstamped room row renders initials even when the nick's holder is known now", () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "bob", "bob@waddle.social");
    expect(resolveAuthorJid({ authorOccupantJid: `${ROOM}/bob`, createdAtSource: "archive" }, directory, SELF)).toBeNull();
    expect(resolveAuthorJid({ authorOccupantJid: `${ROOM}/bob`, createdAtSource: "delay" }, directory, SELF)).toBeNull();
  });

  test("1:1 rows resolve to the peer's bare JID", () => {
    const directory = new OccupantJidDirectory();
    expect(resolveAuthorJid({ authorJid: "alice@other.example/phone" }, directory)).toBe("alice@other.example");
  });

  test("an unstamped MUC private message never resolves to the room JID or the current holder", () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "carol", "carol@waddle.social");
    expect(resolveAuthorJid({ authorJid: `${ROOM}/carol`, authorOccupantJid: `${ROOM}/carol` }, directory)).toBeNull();
  });

  test("a DM peer and a channel member sharing a nick get different avatars", async () => {
    occupantJidDirectory.record(ROOM, "alice", "alice@waddle.social");
    const store = new AvatarStore();
    store.setFetcher(async (jid) => `data:${jid}`);

    const dmJid = authorAvatarJid({ authorJid: "alice@other.example/phone" })!;
    const channelJid = authorAvatarJid(stampLiveRoomAuthor(liveRow("alice"), ROOM, "alice"))!;
    store.retain(dmJid);
    store.retain(channelJid);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(dmJid).toBe("alice@other.example");
    expect(channelJid).toBe("alice@waddle.social");
    expect(store.urlFor(dmJid)).not.toBe(store.urlFor(channelJid));
  });
});

describe("the occupant directory holds only the current holder", () => {
  test("lookups are reactive to newly disclosed occupants and scoped per room", () => {
    const directory = new OccupantJidDirectory();
    const jid = computed(() => directory.lookup(ROOM, "dave"));
    expect(jid.value).toBeNull();
    directory.record(ROOM, "dave", "dave@waddle.social");
    expect(jid.value).toBe("dave@waddle.social");
    expect(directory.lookup("random@muc.waddle.social", "dave")).toBeNull();
  });

  test("a JID-less holder taking the nick makes the current holder unknown", () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "sam", "alice@waddle.social");
    directory.record(ROOM, "sam", null);
    expect(directory.lookup(ROOM, "sam")).toBeNull();
    directory.record(ROOM, "sam", "carol@waddle.social");
    expect(directory.lookup(ROOM, "sam")).toBe("carol@waddle.social");
  });
});

describe("nick handover never changes a row's face", () => {
  test("Alice's live rows keep Alice after Bob takes the nick; Bob's rows get Bob", () => {
    occupantJidDirectory.record(ROOM, "sam", "alice@waddle.social");
    const alices = stampLiveRoomAuthor(liveRow("sam"), ROOM, "sam");
    occupantJidDirectory.record(ROOM, "sam", "bob@waddle.social");
    const bobs = stampLiveRoomAuthor(liveRow("sam"), ROOM, "sam");

    expect(authorAvatarJid(alices)).toBe("alice@waddle.social");
    expect(authorAvatarJid(bobs)).toBe("bob@waddle.social");
    expect(occupantJidDirectory.lookup(ROOM, "sam")).toBe("bob@waddle.social");
  });

  test("delayed and archive rows on the live path are never stamped with the current holder", () => {
    // Alice's row replayed after Bob took the nick: no clock can tell, so initials.
    occupantJidDirectory.record(ROOM, "sam", "bob@waddle.social");
    for (const source of ["delay", "archive"] as const) {
      const row = stampLiveRoomAuthor(liveRow("sam", { createdAtSource: source }), ROOM, "sam");
      expect(row.authorAvatarJid).toBeUndefined();
      expect(authorAvatarJid(row, SELF)).toBeNull();
    }
  });

  test("a hidden (JID-less) holder's live rows stay unstamped; Alice's stamped rows keep Alice", () => {
    occupantJidDirectory.record(ROOM, "sam", "alice@waddle.social");
    const alices = stampLiveRoomAuthor(liveRow("sam"), ROOM, "sam");
    occupantJidDirectory.record(ROOM, "sam", null);
    const hidden = stampLiveRoomAuthor(liveRow("sam"), ROOM, "sam");

    expect(authorAvatarJid(alices)).toBe("alice@waddle.social");
    expect(hidden.authorAvatarJid).toBeUndefined();
    expect(authorAvatarJid(hidden)).toBeNull();
  });

  test("stamping leaves archive real JIDs, existing stamps and unknown occupants alone", () => {
    occupantJidDirectory.record(ROOM, "sam", "alice@waddle.social");
    expect(stampLiveRoomAuthor(liveRow("sam", { authorRealJid: "carol@waddle.social" }), ROOM, "sam").authorAvatarJid).toBeUndefined();
    expect(stampLiveRoomAuthor(liveRow("sam", { authorAvatarJid: "dan@waddle.social" }), ROOM, "sam").authorAvatarJid)
      .toBe("dan@waddle.social");
    expect(stampLiveRoomAuthor(liveRow("nobody"), ROOM, "nobody").authorAvatarJid).toBeUndefined();
  });
});

describe("self attribution never comes from nick equality alone", () => {
  test("an archived row by a past holder of our nick shows the archive's real JID, not our face", () => {
    const directory = new OccupantJidDirectory();
    expect(resolveAuthorJid({
      isSelf: true,
      authorOccupantJid: `${ROOM}/me`,
      authorRealJid: "previous-me@waddle.social",
      createdAtSource: "archive",
    }, directory, SELF)).toBe("previous-me@waddle.social");
  });

  test("an archived same-nick row without a real JID renders initials, not our face", () => {
    const directory = new OccupantJidDirectory();
    directory.recordOwnNick(ROOM, "me");
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me`, createdAtSource: "archive" }, directory, SELF))
      .toBeNull();
  });

  test("our local echoes and live reflections under our actual nick resolve to us", () => {
    const directory = new OccupantJidDirectory();
    directory.recordOwnNick(ROOM, "me");
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me`, deliveryStatus: "sending" }, directory, SELF))
      .toBe("me@waddle.social");
    expect(resolveAuthorJid(liveRow("me", { isSelf: true }), directory, SELF)).toBe("me@waddle.social");
  });
});

describe("room-assigned nick (XEP-0045 210)", () => {
  // We asked for "me"; the room assigned us "me_2". A peer holds "me".
  function assigned() {
    occupantJidDirectory.recordOwnNick(ROOM, "me_2");
    occupantJidDirectory.record(ROOM, "me_2", "me@waddle.social");
    occupantJidDirectory.record(ROOM, "me", "peer@elsewhere.example");
  }

  test("a peer on our requested nick shows the peer's face, even flagged isSelf by nick", () => {
    assigned();
    const peerRow = stampLiveRoomAuthor(liveRow("me", { isSelf: true }), ROOM, "me", SELF);
    expect(peerRow.authorAvatarJid).toBe("peer@elsewhere.example");
    expect(authorAvatarJid(peerRow, SELF)).toBe("peer@elsewhere.example");
  });

  test("our own reflection under the assigned nick is stamped as us, even in a room that hides our JID", () => {
    occupantJidDirectory.recordOwnNick(ROOM, "me_2");
    const own = stampLiveRoomAuthor(liveRow("me_2"), ROOM, "me_2", "Me@Waddle.social/web");
    expect(own.authorAvatarJid).toBe("me@waddle.social");
    // The stamp survives a fresh session forgetting our own nicks.
    occupantJidDirectory.forgetOwnNicks();
    expect(authorAvatarJid(own)).toBe("me@waddle.social");
  });

  test("our local echo stays ours regardless of nick", () => {
    assigned();
    expect(authorAvatarJid({ isSelf: true, authorOccupantJid: `${ROOM}/me`, deliveryStatus: "sending" }, SELF))
      .toBe("me@waddle.social");
  });

  test("a new holder of our old nick is not us between a fresh reconnect and our rejoin self-presence", () => {
    occupantJidDirectory.recordOwnNick(ROOM, "me");
    occupantJidDirectory.forgetOwnNicks();
    occupantJidDirectory.record(ROOM, "me", "peer@elsewhere.example");
    const peerRow = stampLiveRoomAuthor(liveRow("me", { isSelf: true }), ROOM, "me", SELF);
    expect(authorAvatarJid(peerRow, SELF)).toBe("peer@elsewhere.example");
    expect(occupantJidDirectory.ownNick(ROOM)).toBeNull();
  });
});

describe("typing indicator avatars", () => {
  test("a DM peer sharing our localpart on another domain shows the peer, not us", () => {
    // We are alex@a.example; the DM chat-state nick is the peer JID's localpart.
    expect(typingAuthorAvatarJid("alex", { peerJid: "alex@b.example" })).toBe("alex@b.example");
    expect(typingAuthorAvatarJid("alex", { peerJid: "Alex@B.example" })).toBe("alex@b.example");
  });

  test("a MUC private-message peer resolves through the room's current disclosure", () => {
    occupantJidDirectory.record(ROOM, "alex", "alex@b.example");
    expect(typingAuthorAvatarJid("alex", { peerJid: `${ROOM}/alex` })).toBe("alex@b.example");
  });

  test("a room typer is the nick's disclosed holder, us only if that identity is ours", () => {
    expect(typingAuthorAvatarJid("alex", { roomJid: ROOM })).toBeNull();
    occupantJidDirectory.record(ROOM, "alex", "alex@b.example");
    expect(typingAuthorAvatarJid("alex", { roomJid: ROOM })).toBe("alex@b.example");
    occupantJidDirectory.record(ROOM, "me", "me@waddle.social");
    expect(typingAuthorAvatarJid("me", { roomJid: ROOM })).toBe("me@waddle.social");
  });
});

describe("ContentArea wiring", () => {
  test("no nick-equals-self shortcut remains for typing or the profile drawer", async () => {
    const { readFileSync } = await import("node:fs");
    const source = readFileSync(new URL("../src/components/chat/ContentArea.vue", import.meta.url), "utf8");
    expect(source).not.toContain("nick === props.currentUser");
    expect(source).not.toContain("popoverAuthor?.username === currentUser");
    expect(source).toContain("typingAuthorAvatarJid(");
  });
});
