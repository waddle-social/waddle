import { afterEach, describe, expect, test } from "bun:test";
import { computed } from "vue";
import {
  OccupantJidDirectory,
  authorAvatarJid,
  occupantJidDirectory,
  resolveAuthorJid,
  stampLiveRoomAuthor,
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
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me` }, directory, "me@waddle.social/web"))
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
    expect(stampLiveRoomAuthor({ isSelf: true }, ROOM, "sam").authorAvatarJid).toBeUndefined();
    expect(stampLiveRoomAuthor({ authorRealJid: "carol@waddle.social" }, ROOM, "sam").authorAvatarJid).toBeUndefined();
    expect(stampLiveRoomAuthor({}, ROOM, "nobody").authorAvatarJid).toBeUndefined();
  });
});
