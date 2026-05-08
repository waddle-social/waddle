import { describe, expect, test } from "bun:test";
import { pinPermissionFromDiscoFields } from "../src/lib/xmpp/discovery";

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
