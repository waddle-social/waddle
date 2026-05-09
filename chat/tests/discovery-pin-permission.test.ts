import { describe, expect, test } from "bun:test";
import {
  applyDiscoInfoToChannel,
  pinPermissionFromDiscoFields,
  type DiscoInfoData,
} from "../src/lib/xmpp/discovery";
import type { DiscoveredChannel } from "../src/lib/xmpp/types";

describe("pinPermissionFromDiscoFields (#422)", () => {
  test("extracts 'anyone' when the disco field carries that value", () => {
    const fields = new Map([
      ["FORM_TYPE", "urn:waddle:room:0"],
      ["waddle#channel_type", "text"],
      ["urn:waddle:roomconfig:pinpermission", "anyone"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBe("anyone");
  });

  test("extracts 'admins-only' when the disco field carries that value", () => {
    const fields = new Map([
      ["urn:waddle:roomconfig:pinpermission", "admins-only"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBe("admins-only");
  });

  test("returns undefined when the field is absent", () => {
    const fields = new Map([
      ["FORM_TYPE", "urn:waddle:room:0"],
      ["waddle#channel_type", "text"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBeUndefined();
  });

  test("returns undefined when the field carries an unknown value", () => {
    const fields = new Map([
      ["urn:waddle:roomconfig:pinpermission", "open-house"],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBeUndefined();
  });

  test("returns undefined when the field is empty", () => {
    const fields = new Map([
      ["urn:waddle:roomconfig:pinpermission", ""],
    ]);
    expect(pinPermissionFromDiscoFields(fields)).toBeUndefined();
  });
});

/** #422 hydration-path coverage: `applyDiscoInfoToChannel` is the
 * pure transform that maps a parsed disco-info payload onto a
 * `DiscoveredChannel`. `hydrateRoomInfo` is the thin IO wrapper that
 * calls `sendDiscoInfo` (DOMParser path, browser-only) and then
 * delegates here. By exercising this transform directly we lock the
 * stamping contract that `loadStructure` consumes — without needing a
 * DOM polyfill in bun-test. */
describe("applyDiscoInfoToChannel pinPermission (#422)", () => {
  const baseRoom: DiscoveredChannel = {
    id: "general",
    name: "General",
    jid: "general@conference.example.net",
    channelType: "text",
    position: 0,
  };

  function infoWithPin(value: string): DiscoInfoData {
    return {
      features: ["http://jabber.org/protocol/muc"],
      identities: [{ category: "conference", type: "text", name: "General" }],
      fields: new Map([
        ["FORM_TYPE", "urn:waddle:room:0"],
        ["waddle#channel_type", "text"],
        ["urn:waddle:roomconfig:pinpermission", value],
      ]),
    };
  }

  test("stamps pinPermission='anyone' onto the channel", () => {
    const hydrated = applyDiscoInfoToChannel(baseRoom, infoWithPin("anyone"));
    expect(hydrated.pinPermission).toBe("anyone");
  });

  test("stamps pinPermission='admins-only' onto the channel", () => {
    const hydrated = applyDiscoInfoToChannel(baseRoom, infoWithPin("admins-only"));
    expect(hydrated.pinPermission).toBe("admins-only");
  });

  test("leaves pinPermission undefined when the disco field is absent", () => {
    const hydrated = applyDiscoInfoToChannel(baseRoom, {
      features: ["http://jabber.org/protocol/muc"],
      identities: [],
      fields: new Map(),
    });
    expect(hydrated.pinPermission).toBeUndefined();
  });
});
