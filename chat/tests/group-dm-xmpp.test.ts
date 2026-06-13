import { describe, expect, test } from "bun:test";
import {
  buildCreateGroupDmCommandXml,
  GROUP_DM_CREATE_NODE,
  parseCreateGroupDmResult,
} from "../src/lib/xmpp/group-dm";

describe("group-DM XEP-0050 helper", () => {
  test("builds a submit form with jid-multi members", () => {
    const xml = buildCreateGroupDmCommandXml({
      userJid: "alice@example.com/browser",
      name: "Alice, Bob, Carol",
      memberJids: ["bob@example.com", "carol@example.com"],
    });

    expect(xml).toContain(`node="${GROUP_DM_CREATE_NODE}"`);
    expect(xml).toContain(`action="execute"`);
    expect(xml).toContain(`to="example.com"`);
    expect(xml).toContain(`<field var="FORM_TYPE" type="hidden"><value>${GROUP_DM_CREATE_NODE}</value></field>`);
    expect(xml).toContain(`<field var="member_jids" type="jid-multi"><value>bob@example.com</value><value>carol@example.com</value></field>`);
  });

  test("parses the room_jid result field", () => {
    const result = parseCreateGroupDmResult(`
      <iq type="result">
        <command xmlns="http://jabber.org/protocol/commands" status="completed">
          <x xmlns="jabber:x:data" type="result">
            <field var="room_jid"><value>group-dm-rock@muc.example.com</value></field>
          </x>
        </command>
      </iq>
    `);

    expect(result).toEqual({ roomJid: "group-dm-rock@muc.example.com" });
  });
});
