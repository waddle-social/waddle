import { describe, expect, test } from "bun:test";
import {
  buildSpaceFederatedSubmission,
  FIELD_SPACE_FEDERATED,
  fetchRoomFederated,
  fetchSpaceFederated,
  formBooleanField,
  parseFormBoolean,
  setSpaceFederated,
} from "../src/lib/xmpp/federated-settings";
import { configureMucRoom, FIELD_ROOM_FEDERATED } from "../src/lib/xmpp/protocol-helpers";
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

function withXml(run: () => Promise<void>): Promise<void> {
  return withFakeDomParser(() => withFakeXmlDocument(run));
}

function ownerFormXml(fields: Record<string, string>): string {
  return `<iq type="result"><query xmlns="http://jabber.org/protocol/muc#owner"><x xmlns="jabber:x:data" type="form">${
    Object.entries(fields).map(([name, value]) => `<field var="${name}"><value>${value}</value></field>`).join("")
  }</x></query></iq>`;
}

function spaceConfigureXml(federated: string | null): string {
  return `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub#owner"><configure node="eng"><x xmlns="jabber:x:data" type="form">`
    + `<field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field>`
    + `<field var="pubsub#title"><value>Engineering</value></field>`
    + (federated === null ? "" : `<field var="${FIELD_SPACE_FEDERATED}" type="boolean"><value>${federated}</value></field>`)
    + `</x></configure></pubsub></iq>`;
}

describe("form booleans", () => {
  test("XEP-0004 boolean values", () => {
    expect(parseFormBoolean("1")).toBe(true);
    expect(parseFormBoolean("true")).toBe(true);
    expect(parseFormBoolean("0")).toBe(false);
    expect(parseFormBoolean("false")).toBe(false);
    expect(parseFormBoolean("yes")).toBeNull();
  });

  test("a field the form does not offer reads as not offered", async () => {
    await withFakeDomParser(async () => {
      const doc = new DOMParser().parseFromString(ownerFormXml({ "muc#roomconfig_roomname": "Dev" }), "text/xml");
      expect(formBooleanField(doc, FIELD_ROOM_FEDERATED)).toBeNull();
    });
  });
});

describe("a room's federated setting", () => {
  test("is read from the owner form, off unless the server says on", async () => {
    await withXml(async () => {
      const off = { async send_raw_iq() { return ownerFormXml({ [FIELD_ROOM_FEDERATED]: "0" }); } };
      const on = { async send_raw_iq() { return ownerFormXml({ [FIELD_ROOM_FEDERATED]: "1" }); } };
      const absent = { async send_raw_iq() { return ownerFormXml({}); } };
      expect(await fetchRoomFederated(off, "dev@muc.home.test")).toBe(false);
      expect(await fetchRoomFederated(on, "dev@muc.home.test")).toBe(true);
      expect(await fetchRoomFederated(absent, "dev@muc.home.test")).toBeNull();
    });
  });

  test("is written only when asked and only when the form offers it", async () => {
    await withFakeDomParser(async () => {
      const submitted: string[] = [];
      const offering = {
        async send_raw_iq(xml: string) {
          if (xml.includes('type="get"')) return ownerFormXml({ "muc#roomconfig_roomname": "Dev", [FIELD_ROOM_FEDERATED]: "0" });
          submitted.push(xml);
          return '<iq type="result"/>';
        },
      };
      await configureMucRoom(offering, "dev@muc.home.test", { name: "Dev", federated: true });
      expect(submitted[0]).toContain(`<field var="${FIELD_ROOM_FEDERATED}"><value>1</value></field>`);

      await configureMucRoom(offering, "dev@muc.home.test", { name: "Dev" });
      expect(submitted[1]).toContain(`<field var="${FIELD_ROOM_FEDERATED}"><value>0</value></field>`);

      const notOffering = {
        async send_raw_iq(xml: string) {
          if (xml.includes('type="get"')) return ownerFormXml({ "muc#roomconfig_roomname": "Dev" });
          submitted.push(xml);
          return '<iq type="result"/>';
        },
      };
      await configureMucRoom(notOffering, "dev@muc.home.test", { name: "Dev", federated: true });
      expect(submitted[2]).not.toContain(FIELD_ROOM_FEDERATED);
    });
  });
});

describe("a Space's federated setting", () => {
  test("the change submits only the federated field", async () => {
    await withXml(async () => {
      const xml = buildSpaceFederatedSubmission("c1", "spaces.home.test", "eng", true);
      expect(xml).toContain('to="spaces.home.test"');
      expect(xml).toContain('<pubsub xmlns="http://jabber.org/protocol/pubsub#owner"><configure node="eng">');
      expect(xml).toContain(`<field var="${FIELD_SPACE_FEDERATED}"><value>1</value></field>`);
      expect(xml).not.toContain("pubsub#title");
    });
  });

  test("is read from the configure form, and not offered when the field is absent", async () => {
    await withXml(async () => {
      expect(await fetchSpaceFederated({ async send_raw_iq() { return spaceConfigureXml("1"); } }, "spaces.home.test", "eng")).toBe(true);
      expect(await fetchSpaceFederated({ async send_raw_iq() { return spaceConfigureXml(null); } }, "spaces.home.test", "eng")).toBeNull();
    });
  });

  test("after a change the server's value is shown, not the submitted one", async () => {
    await withXml(async () => {
      const refused = Object.assign(new Error("forbidden"), { condition: "forbidden", errorType: "auth" });
      const xmpp = {
        async send_raw_iq(xml: string) {
          if (xml.includes('type="set"')) throw refused;
          return spaceConfigureXml("0");
        },
      };
      const result = await setSpaceFederated(xmpp, "spaces.home.test", "eng", true);
      expect(result.current).toBe(false);
      expect(result.error).toBe(refused);
    });
  });
});
