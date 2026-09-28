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
const T0 = Date.parse("2026-09-27T10:00:00Z");
const at = (minutes: number) => new Date(T0 + minutes * 60_000).toISOString();

/** Directory whose clock the test moves. */
function directoryAt(startMinutes = 0) {
  let now = T0 + startMinutes * 60_000;
  const directory = new OccupantJidDirectory(() => now);
  return { directory, setMinutes: (minutes: number) => { now = T0 + minutes * 60_000; } };
}

afterEach(() => occupantJidDirectory.clear());

describe("resolveAuthorJid", () => {
  test("self rows resolve to our own bare JID", () => {
    const { directory } = directoryAt();
    directory.recordOwnNick(ROOM, "me");
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me`, createdAtSource: "fallback" }, directory, "me@waddle.social/web"))
      .toBe("me@waddle.social");
  });

  test("the MUC archive real JID wins", () => {
    const { directory } = directoryAt();
    directory.record(ROOM, "alice", "someone-else@waddle.social");
    expect(resolveAuthorJid({
      authorOccupantJid: `${ROOM}/alice`,
      authorRealJid: "Alice@Waddle.social/phone",
      createdAt: at(5),
    }, directory)).toBe("alice@waddle.social");
  });

  test("room rows resolve through the disclosure in effect when they were sent", () => {
    const { directory } = directoryAt();
    directory.record(ROOM, "alice", "alice@waddle.social/laptop");
    expect(resolveAuthorJid({ authorJid: `${ROOM}/alice`, authorOccupantJid: `${ROOM}/alice`, createdAt: at(1) }, directory))
      .toBe("alice@waddle.social");
  });

  test("the mapping is retained after the occupant leaves and is scoped per room", () => {
    const { directory } = directoryAt();
    directory.record(ROOM, "alice", "alice@waddle.social");
    // No removal API: departures keep history resolvable.
    expect(directory.lookup(ROOM, "alice")).toBe("alice@waddle.social");
    expect(directory.lookup("random@muc.waddle.social", "alice")).toBeNull();
  });

  test("an unresolved nick renders initials — no nick@domain guess", () => {
    const { directory } = directoryAt();
    expect(resolveAuthorJid({ authorJid: `${ROOM}/bob`, authorOccupantJid: `${ROOM}/bob`, createdAt: at(1) }, directory, "me@waddle.social"))
      .toBeNull();
  });

  test("1:1 rows resolve to the peer's bare JID", () => {
    const { directory } = directoryAt();
    expect(resolveAuthorJid({ authorJid: "alice@other.example/phone" }, directory)).toBe("alice@other.example");
  });

  test("MUC private messages resolve through the room, never as a room-JID person", () => {
    const { directory } = directoryAt();
    const pm = { authorJid: `${ROOM}/carol`, authorOccupantJid: `${ROOM}/carol`, createdAt: at(2) };
    expect(resolveAuthorJid(pm, directory)).toBeNull();
    directory.record(ROOM, "carol", "carol@waddle.social");
    expect(resolveAuthorJid(pm, directory)).toBe("carol@waddle.social");
  });

  test("a DM peer and a channel member sharing a nick get different avatars", async () => {
    const { directory } = directoryAt();
    directory.record(ROOM, "alice", "alice@waddle.social");
    const store = new AvatarStore();
    store.setFetcher(async (jid) => `data:${jid}`);

    const dmRow = { authorJid: "alice@other.example/phone" };
    const channelRow = { authorJid: `${ROOM}/alice`, authorOccupantJid: `${ROOM}/alice`, createdAt: at(1) };
    const dmJid = resolveAuthorJid(dmRow, directory)!;
    const channelJid = resolveAuthorJid(channelRow, directory)!;
    store.retain(dmJid);
    store.retain(channelJid);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(dmJid).toBe("alice@other.example");
    expect(channelJid).toBe("alice@waddle.social");
    expect(store.urlFor(dmJid)).toBe("data:alice@other.example");
    expect(store.urlFor(channelJid)).toBe("data:alice@waddle.social");
  });

  test("lookups are reactive to newly disclosed occupants", () => {
    const { directory } = directoryAt();
    const jid = computed(() => directory.lookup(ROOM, "dave"));
    expect(jid.value).toBeNull();
    directory.record(ROOM, "dave", "dave@waddle.social");
    expect(jid.value).toBe("dave@waddle.social");
  });
});

describe("nick reuse never re-attributes past rows", () => {
  test("Alice posts as sam and leaves; Bob joins as sam — Alice's rows keep Alice's face", () => {
    const { directory, setMinutes } = directoryAt();
    directory.record(ROOM, "sam", "alice@waddle.social");
    const aliceRow = { authorJid: `${ROOM}/sam`, authorOccupantJid: `${ROOM}/sam`, createdAt: at(1) };
    expect(resolveAuthorJid(aliceRow, directory)).toBe("alice@waddle.social");

    // Alice leaves (mapping retained); later Bob takes the nick.
    setMinutes(10);
    directory.record(ROOM, "sam", "bob@waddle.social");
    const bobRow = { authorJid: `${ROOM}/sam`, authorOccupantJid: `${ROOM}/sam`, createdAt: at(11) };

    expect(resolveAuthorJid(aliceRow, directory)).toBe("alice@waddle.social");
    expect(resolveAuthorJid(bobRow, directory)).toBe("bob@waddle.social");
    // Live occupant surfaces (typing, presence stack) see who is there now.
    expect(directory.lookup(ROOM, "sam")).toBe("bob@waddle.social");
  });

  test("a row sent before any disclosure stays initials when the nick is later mapped", () => {
    const { directory, setMinutes } = directoryAt();
    const earlyRow = { authorOccupantJid: `${ROOM}/sam`, createdAt: at(1) };
    setMinutes(10);
    directory.record(ROOM, "sam", "bob@waddle.social");
    expect(resolveAuthorJid(earlyRow, directory)).toBeNull();
  });

  test("live rows are stamped at ingest with the occupant behind the nick at that moment", () => {
    occupantJidDirectory.record(ROOM, "sam", "alice@waddle.social");
    // A skewed or unparseable timestamp cannot unstamp a live row.
    const aliceRow = stampLiveRoomAuthor(
      { authorJid: `${ROOM}/sam`, authorOccupantJid: `${ROOM}/sam`, createdAt: "not-a-timestamp" },
      ROOM,
      "sam",
    );
    expect(aliceRow.authorAvatarJid).toBe("alice@waddle.social");

    occupantJidDirectory.record(ROOM, "sam", "bob@waddle.social");
    expect(authorAvatarJid(aliceRow)).toBe("alice@waddle.social");
    const bobRow = stampLiveRoomAuthor({ authorOccupantJid: `${ROOM}/sam`, createdAt: at(11) }, ROOM, "sam");
    expect(authorAvatarJid(bobRow)).toBe("bob@waddle.social");
  });

  test("stamping leaves self rows, archive real JIDs and unknown occupants alone", () => {
    occupantJidDirectory.record(ROOM, "sam", "alice@waddle.social");
    expect(stampLiveRoomAuthor({ isSelf: true, createdAtSource: "fallback" }, ROOM, "sam").authorAvatarJid).toBeUndefined();
    expect(stampLiveRoomAuthor({ authorRealJid: "carol@waddle.social" }, ROOM, "sam").authorAvatarJid).toBeUndefined();
    expect(stampLiveRoomAuthor({}, ROOM, "nobody").authorAvatarJid).toBeUndefined();
  });
});

describe("self attribution never comes from nick equality alone", () => {
  test("an archived row by a past holder of our nick shows the archive's real JID, not our face", () => {
    const { directory } = directoryAt();
    expect(resolveAuthorJid({
      isSelf: true,
      authorOccupantJid: `${ROOM}/me`,
      authorRealJid: "previous-me@waddle.social",
      createdAt: at(1),
      createdAtSource: "archive",
    }, directory, "me@waddle.social/web")).toBe("previous-me@waddle.social");
  });

  test("an archived same-nick row without a real JID uses the mapping of its time, not our JID", () => {
    const { directory, setMinutes } = directoryAt();
    directory.record(ROOM, "me", "previous-me@waddle.social");
    const past = { isSelf: true, authorOccupantJid: `${ROOM}/me`, createdAt: at(1), createdAtSource: "archive" as const };
    setMinutes(10);
    directory.record(ROOM, "me", "me@waddle.social");
    expect(resolveAuthorJid(past, directory, "me@waddle.social/web")).toBe("previous-me@waddle.social");
    // Nobody mapped at the time: initials rather than our face.
    expect(resolveAuthorJid({ ...past, createdAt: at(-5) }, directory, "me@waddle.social/web")).toBeNull();
  });

  test("our own sends and live echoes still resolve to us", () => {
    const { directory } = directoryAt();
    directory.recordOwnNick(ROOM, "me");
    const selfJid = "me@waddle.social/web";
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me`, deliveryStatus: "sending" }, directory, selfJid))
      .toBe("me@waddle.social");
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me`, createdAtSource: "fallback", createdAt: at(1) }, directory, selfJid))
      .toBe("me@waddle.social");
  });

  test("stamping still pins a past same-nick row that merely matches our nick", () => {
    occupantJidDirectory.record(ROOM, "me", "previous-me@waddle.social");
    const row = stampLiveRoomAuthor(
      { isSelf: true, authorOccupantJid: `${ROOM}/me`, createdAt: new Date(Date.now() + 1000).toISOString(), createdAtSource: "archive" },
      ROOM,
      "me",
    );
    expect(row.authorAvatarJid).toBe("previous-me@waddle.social");
  });
});

describe("handover to an occupant whose real JID is hidden", () => {
  test("the current holder becomes unknown while earlier rows keep the earlier holder", () => {
    const { directory, setMinutes } = directoryAt();
    directory.record(ROOM, "sam", "alice@waddle.social");
    const alicesRow = { authorOccupantJid: `${ROOM}/sam`, createdAt: at(1) };

    setMinutes(10);
    directory.record(ROOM, "sam", null);

    expect(directory.lookup(ROOM, "sam")).toBeNull();
    expect(resolveAuthorJid(alicesRow, directory)).toBe("alice@waddle.social");
    const hiddenHoldersRow = { authorOccupantJid: `${ROOM}/sam`, createdAt: at(11) };
    expect(resolveAuthorJid(hiddenHoldersRow, directory)).toBeNull();

    // A later disclosed holder is recorded as usual.
    setMinutes(20);
    directory.record(ROOM, "sam", "carol@waddle.social");
    expect(directory.lookup(ROOM, "sam")).toBe("carol@waddle.social");
    expect(resolveAuthorJid(hiddenHoldersRow, directory)).toBeNull();
    expect(resolveAuthorJid(alicesRow, directory)).toBe("alice@waddle.social");
  });

  test("new live rows by the hidden holder stay unstamped; Alice's stamped rows keep Alice", () => {
    occupantJidDirectory.record(ROOM, "sam", "alice@waddle.social");
    const alicesLive = stampLiveRoomAuthor({ authorOccupantJid: `${ROOM}/sam`, createdAtSource: "fallback" }, ROOM, "sam");
    occupantJidDirectory.record(ROOM, "sam", null);
    const hiddenLive = stampLiveRoomAuthor({ authorOccupantJid: `${ROOM}/sam`, createdAtSource: "fallback" }, ROOM, "sam");

    expect(alicesLive.authorAvatarJid).toBe("alice@waddle.social");
    expect(hiddenLive.authorAvatarJid).toBeUndefined();
    expect(authorAvatarJid(alicesLive)).toBe("alice@waddle.social");
  });

  test("a JID-less presence for a nick never seen before records nothing", () => {
    const { directory } = directoryAt();
    directory.record(ROOM, "ghost", null);
    expect(directory.lookup(ROOM, "ghost")).toBeNull();
    directory.record(ROOM, "ghost", "ghost@waddle.social");
    expect(directory.lookup(ROOM, "ghost")).toBe("ghost@waddle.social");
  });
});

describe("typing indicator avatars", () => {
  test("a DM peer sharing our localpart on another domain shows the peer, not us", () => {
    // We are alex@a.example; the DM chat-state nick is the peer JID's localpart.
    expect(typingAuthorAvatarJid("alex", { peerJid: "alex@b.example" })).toBe("alex@b.example");
    expect(typingAuthorAvatarJid("alex", { peerJid: "Alex@B.example" })).toBe("alex@b.example");
  });

  test("a MUC private-message peer resolves through the room's disclosure", () => {
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

describe("room-assigned nick (XEP-0045 210)", () => {
  // We asked for "me"; the room assigned us "me_2". A peer holds "me".
  function assigned() {
    const { directory } = directoryAt();
    directory.recordOwnNick(ROOM, "me_2");
    directory.record(ROOM, "me_2", "me@waddle.social");
    directory.record(ROOM, "me", "peer@elsewhere.example");
    return directory;
  }
  const selfJid = "me@waddle.social/web";

  test("a peer on our requested nick shows the peer's face, even flagged isSelf by nick", () => {
    const directory = assigned();
    // timeline.ts marks this row isSelf (nick === username); it is the peer's.
    const peerRow = { isSelf: true, authorOccupantJid: `${ROOM}/me`, createdAtSource: "fallback" as const, createdAt: at(1) };
    expect(resolveAuthorJid(peerRow, directory, selfJid)).toBe("peer@elsewhere.example");
    // The peer's disclosed stamp also beats the own-send fallback.
    expect(resolveAuthorJid({ ...peerRow, authorAvatarJid: "peer@elsewhere.example" }, directory, selfJid))
      .toBe("peer@elsewhere.example");
  });

  test("our own reflection under the assigned nick resolves to us", () => {
    const directory = assigned();
    const ownReflection = { isSelf: false, authorOccupantJid: `${ROOM}/me_2`, createdAtSource: "fallback" as const, createdAt: at(1) };
    expect(resolveAuthorJid(ownReflection, directory, selfJid)).toBe("me@waddle.social");
    // Even in a room that does not disclose our JID.
    const anonymous = directoryAt().directory;
    anonymous.recordOwnNick(ROOM, "me_2");
    expect(resolveAuthorJid(ownReflection, anonymous, selfJid)).toBe("me@waddle.social");
  });

  test("our local echo stays ours regardless of nick", () => {
    const directory = assigned();
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me`, deliveryStatus: "sending" }, directory, selfJid))
      .toBe("me@waddle.social");
  });

  test("live stamping pins the peer on our requested nick and skips our own reflection", () => {
    occupantJidDirectory.recordOwnNick(ROOM, "me_2");
    occupantJidDirectory.record(ROOM, "me", "peer@elsewhere.example");
    occupantJidDirectory.record(ROOM, "me_2", "me@waddle.social");
    const peerRow = stampLiveRoomAuthor({ isSelf: true, authorOccupantJid: `${ROOM}/me`, createdAtSource: "fallback" }, ROOM, "me");
    expect(peerRow.authorAvatarJid).toBe("peer@elsewhere.example");
    expect(authorAvatarJid(peerRow, selfJid)).toBe("peer@elsewhere.example");
    const own = stampLiveRoomAuthor({ authorOccupantJid: `${ROOM}/me_2`, createdAtSource: "fallback" }, ROOM, "me_2");
    expect(own.authorAvatarJid).toBeUndefined();
    expect(authorAvatarJid(own, selfJid)).toBe("me@waddle.social");
  });
});

