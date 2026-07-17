import { describe, expect, mock, test } from "bun:test";
import {
  configureMucRoom,
  createMucRoom,
  createSpaceNode,
  moveMucToSpace,
} from "../src/lib/xmpp/protocol-helpers";
import { withFakeDomParser } from "./helpers/disco-xml";

describe("protocol helpers", () => {
  test("creates MUC rooms via join_room (always no-history per #1255)", async () => {
    const joinRoom = mock(async () => undefined);
    const leaveRoom = mock(async () => undefined);
    const sendRawIq = mock(async () => "<iq type='result'/>");
    const client = {
      join_room: joinRoom,
      leave_room: leaveRoom,
      send_raw_iq: sendRawIq,
    };

    await createMucRoom(client, "rooms.example.com", {
      roomLocalpart: "general",
      nick: "alice",
      name: "General",
      description: "Default room",
    });

    // #1255: `join_room` itself sends `<history maxstanzas='0'/>`
    // (asserted in the Rust XEP-0045 suite), so the helper needs no
    // separate no-history variant.
    expect(joinRoom).toHaveBeenCalledWith("general@rooms.example.com", "alice");
    expect(sendRawIq).toHaveBeenCalledTimes(1);
    expect(leaveRoom).toHaveBeenCalledWith("general@rooms.example.com", "alice");
  });

  test("round-trips the XEP-0045 owner form before applying room overrides", async () => {
    const ownerForm = [
      '<iq type="result">',
      '<query xmlns="http://jabber.org/protocol/muc#owner">',
      '<x xmlns="jabber:x:data" type="form">',
      '<field var="FORM_TYPE"><value>http://jabber.org/protocol/muc#roomconfig</value></field>',
      '<field var="muc#roomconfig_roomname"><value>Old name</value></field>',
      '<field var="muc#roomconfig_membersonly"><value>1</value></field>',
      "</x>",
      "</query>",
      "</iq>",
    ].join("");
    const sendRawIq = mock(async (xml: string) =>
      xml.includes('type="get"') ? ownerForm : '<iq type="result"/>'
    );

    await withFakeDomParser(async () => {
      await configureMucRoom(
        { send_raw_iq: sendRawIq },
        "general@rooms.example.com",
        {
          name: "General & Help",
          description: "Default <room>",
          pinPermission: "admins",
        },
      );
    });

    expect(sendRawIq).toHaveBeenCalledTimes(2);
    const submitted = sendRawIq.mock.calls[1]![0];
    expect(submitted).toContain('<query xmlns="http://jabber.org/protocol/muc#owner">');
    expect(submitted).toContain('<field var="muc#roomconfig_membersonly"><value>1</value></field>');
    expect(submitted).toContain('<field var="muc#roomconfig_roomname"><value>General &amp; Help</value></field>');
    expect(submitted).toContain('<field var="muc#roomconfig_roomdesc"><value>Default &lt;room&gt;</value></field>');
    expect(submitted).toContain('<field var="urn:waddle:roomconfig:pinpermission"><value>admins</value></field>');
  });

  test("creates and updates XEP-0503 Space membership with bookmark payloads", async () => {
    const sendRawIq = mock(async () => '<iq type="result"/>');
    const client = { send_raw_iq: sendRawIq };

    expect(await createSpaceNode(client, "spaces.example.com", {
      nodeId: "engineering",
      name: "Engineering",
      description: "Product engineering",
    })).toEqual({
      node: "engineering",
      serviceJid: "spaces.example.com",
    });
    await moveMucToSpace(
      client,
      "spaces.example.com",
      "engineering",
      "platform@rooms.example.com",
      { name: "Platform", autojoin: true },
    );

    const createXml = sendRawIq.mock.calls[0]![0];
    expect(createXml).toContain('<pubsub xmlns="http://jabber.org/protocol/pubsub">');
    expect(createXml).toContain('<create node="engineering"/>');
    expect(createXml).toContain('<field var="pubsub#type"><value>urn:xmpp:spaces:0</value></field>');

    const publishXml = sendRawIq.mock.calls[1]![0];
    expect(publishXml).toContain('<publish node="engineering">');
    expect(publishXml).toContain('<item id="platform@rooms.example.com">');
    expect(publishXml).toContain('<conference xmlns="urn:xmpp:bookmarks:1" name="Platform" autojoin="true"/>');
  });
});
