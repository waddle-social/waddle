import { describe, expect, test } from "bun:test";
import type { MemberSummary } from "../src/lib/chat-types";
import {
  mergeOccupantHats,
  mergeRoomHats,
  roleHatsForOccupant,
  roomHatsFromMembers,
} from "../src/lib/xmpp/occupant-badges";

describe("occupant badge derivation", () => {
  test("derives owner, admin, and moderator hats from MUC metadata", () => {
    expect(roleHatsForOccupant("owner").map((hat) => hat.title)).toEqual(["Owner"]);
    expect(roleHatsForOccupant("admin", "moderator").map((hat) => hat.title)).toEqual(["Admin", "Moderator"]);
    expect(roleHatsForOccupant("member", "participant")).toEqual([]);
  });

  test("deduplicates synthetic and XEP-0317 hats by uri", () => {
    expect(
      mergeOccupantHats(
        roleHatsForOccupant("owner"),
        [
          { uri: "urn:xmpp:hats:owner", title: "Room Owner" },
          { uri: "urn:example:hats:founder", title: "Founder" },
        ],
      ),
    ).toEqual([
      { uri: "urn:xmpp:hats:owner", title: "Owner" },
      { uri: "urn:example:hats:founder", title: "Founder" },
    ]);
  });

  test("merges member-list affiliation badges with live presence hats", () => {
    const members: MemberSummary[] = [
      { jid: "alice@example.com", username: "Alice", avatar_url: null, role: "owner", joined_at: "" },
      { jid: "bob@example.com", username: "Bob", avatar_url: null, role: "member", joined_at: "" },
    ];

    expect(mergeRoomHats(roomHatsFromMembers(members), {
      Alice: [{ uri: "urn:example:hats:verified", title: "Verified" }],
      Bob: [{ uri: "urn:xmpp:hats:moderator", title: "Moderator" }],
    })).toEqual({
      Alice: [
        { uri: "urn:xmpp:hats:owner", title: "Owner" },
        { uri: "urn:example:hats:verified", title: "Verified" },
      ],
      Bob: [{ uri: "urn:xmpp:hats:moderator", title: "Moderator" }],
    });
  });
});
