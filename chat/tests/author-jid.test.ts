import { describe, expect, test } from "bun:test";
import { computed } from "vue";
import { OccupantJidDirectory, resolveAuthorJid } from "../src/lib/avatars/author-jid";
import { AvatarStore } from "../src/lib/avatars/avatar-store";

const ROOM = "general@muc.waddle.social";

describe("resolveAuthorJid", () => {
  test("self rows resolve to our own bare JID", () => {
    const directory = new OccupantJidDirectory();
    expect(resolveAuthorJid({ isSelf: true, authorOccupantJid: `${ROOM}/me` }, directory, "me@waddle.social/web"))
      .toBe("me@waddle.social");
  });

  test("the MUC archive real JID wins", () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "alice", "someone-else@waddle.social");
    expect(resolveAuthorJid({
      authorOccupantJid: `${ROOM}/alice`,
      authorRealJid: "Alice@Waddle.social/phone",
    }, directory)).toBe("alice@waddle.social");
  });

  test("room rows resolve through the occupant's disclosed real JID", () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "alice", "alice@waddle.social/laptop");
    expect(resolveAuthorJid({ authorJid: `${ROOM}/alice`, authorOccupantJid: `${ROOM}/alice` }, directory))
      .toBe("alice@waddle.social");
  });

  test("the mapping is retained after the occupant leaves and is scoped per room", () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "alice", "alice@waddle.social");
    // No removal API: departures keep history resolvable.
    expect(directory.lookup(ROOM, "alice")).toBe("alice@waddle.social");
    expect(directory.lookup("random@muc.waddle.social", "alice")).toBeNull();
  });

  test("an unresolved nick renders initials — no nick@domain guess", () => {
    const directory = new OccupantJidDirectory();
    expect(resolveAuthorJid({ authorJid: `${ROOM}/bob`, authorOccupantJid: `${ROOM}/bob` }, directory, "me@waddle.social"))
      .toBeNull();
  });

  test("1:1 rows resolve to the peer's bare JID", () => {
    const directory = new OccupantJidDirectory();
    expect(resolveAuthorJid({ authorJid: "alice@other.example/phone" }, directory)).toBe("alice@other.example");
  });

  test("MUC private messages resolve through the room, never as a room-JID person", () => {
    const directory = new OccupantJidDirectory();
    const pm = { authorJid: `${ROOM}/carol`, authorOccupantJid: `${ROOM}/carol` };
    expect(resolveAuthorJid(pm, directory)).toBeNull();
    directory.record(ROOM, "carol", "carol@waddle.social");
    expect(resolveAuthorJid(pm, directory)).toBe("carol@waddle.social");
  });

  test("a DM peer and a channel member sharing a nick get different avatars", async () => {
    const directory = new OccupantJidDirectory();
    directory.record(ROOM, "alice", "alice@waddle.social");
    const store = new AvatarStore();
    store.setFetcher(async (jid) => `data:${jid}`);

    const dmRow = { authorJid: "alice@other.example/phone" };
    const channelRow = { authorJid: `${ROOM}/alice`, authorOccupantJid: `${ROOM}/alice` };
    const dmJid = resolveAuthorJid(dmRow, directory)!;
    const channelJid = resolveAuthorJid(channelRow, directory)!;
    store.retain(dmJid);
    store.retain(channelJid);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(dmJid).toBe("alice@other.example");
    expect(channelJid).toBe("alice@waddle.social");
    expect(store.urlFor(dmJid)).toBe("data:alice@other.example");
    expect(store.urlFor(channelJid)).toBe("data:alice@waddle.social");
    expect(store.urlFor(dmJid)).not.toBe(store.urlFor(channelJid));
  });

  test("lookups are reactive to newly disclosed occupants", () => {
    const directory = new OccupantJidDirectory();
    const jid = computed(() => resolveAuthorJid({ authorOccupantJid: `${ROOM}/dave` }, directory));
    expect(jid.value).toBeNull();
    directory.record(ROOM, "dave", "dave@waddle.social");
    expect(jid.value).toBe("dave@waddle.social");
  });
});
