import { describe, expect, mock, test } from "bun:test";
import { configureMucRoom, createMucRoom, createSpaceNode } from "../src/lib/xmpp/protocol-helpers";
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

function withXml(run: () => Promise<void>): Promise<void> {
  return withFakeDomParser(() => withFakeXmlDocument(run));
}

describe("protocol helpers", () => {
  test("creates MUC rooms via join_room (always no-history per #1255)", async () => withXml(async () => {
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
  }));

  test("requests are built with the DOM, so addresses and values are escaped", async () => withXml(async () => {
    const sent: string[] = [];
    const client = {
      async send_raw_iq(xml: string) {
        sent.push(xml);
        return xml.includes('type="get"')
          ? '<iq type="result"><query xmlns="http://jabber.org/protocol/muc#owner"><x xmlns="jabber:x:data" type="form"></x></query></iq>'
          : "<iq type='result'/>";
      },
    };
    await configureMucRoom(client, 'dev@muc.remote.test" evil="1', { name: 'Dev <&> "team"' });
    expect(sent[0]).toContain('to="dev@muc.remote.test&quot; evil=&quot;1"');
    expect(sent[1]).toContain('<value>Dev &lt;&amp;&gt; &quot;team&quot;</value>');
    expect(sent.join("")).not.toContain('evil="1"');

    await createSpaceNode(client, "spaces.home.test", { nodeId: 'eng"x', name: "Engineering" });
    expect(sent[2]).toContain('<create node="eng&quot;x">');
    expect(sent[2]).toContain('<configure node="eng&quot;x">');
  }));
});
