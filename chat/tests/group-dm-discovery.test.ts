import { describe, expect, test } from "bun:test";
import { applyDiscoInfoToChannel, isGroupDmDiscoInfo } from "../src/lib/xmpp/discovery";

describe("group-DM discovery classification", () => {
  test("classifies rooms by urn:waddle:group-dm:0 disco feature", () => {
    const info = {
      features: ["http://jabber.org/protocol/muc", "urn:waddle:group-dm:0"],
      identities: [],
      fields: new Map<string, string>(),
      forms: [],
    };

    expect(isGroupDmDiscoInfo(info)).toBe(true);
    expect(applyDiscoInfoToChannel({
      id: "group-dm-rock",
      name: "Rock",
      jid: "group-dm-rock@muc.example.com",
      channelType: "text",
      position: 0,
    }, info).isGroupDm).toBe(true);
  });
});
