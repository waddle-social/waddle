import { describe, expect, test } from "bun:test";
import { groupDmSpawnPayloadFromDm } from "@/dms/group-dm-spawn";

describe("group DM spawn from a direct message", () => {
  test("builds a fresh group-DM create payload from the DM peer plus picked people", () => {
    const payload = groupDmSpawnPayloadFromDm({
      peerJid: "bob@example.com/laptop",
      selfJid: "alice@example.com/browser",
      selectedMemberJids: [
        "carol@example.com",
        "bob@example.com/phone",
        "alice@example.com/tablet",
        "dave@example.com",
        "carol@example.com/mobile",
      ],
      selectedMemberLabels: ["Carol", "Dave"],
    });

    expect(payload).toEqual({
      name: "Bob, Carol, Dave",
      memberJids: ["bob@example.com", "carol@example.com", "dave@example.com"],
    });
  });

  test("uses an explicit room name when the initiator provides one", () => {
    const payload = groupDmSpawnPayloadFromDm({
      peerJid: "bob@example.com",
      name: "Project launch",
      selectedMemberJids: ["carol@example.com"],
    });

    expect(payload.name).toBe("Project launch");
    expect(payload.memberJids).toEqual(["bob@example.com", "carol@example.com"]);
  });
});
