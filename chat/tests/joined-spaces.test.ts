import { describe, expect, test } from "bun:test";
import {
  buildLoadJoinedSpacesIq,
  buildSaveJoinedSpacesIq,
  joinedSpaceKey,
  parseJoinedSpaces,
  privateXmlJoinedSpaceStore,
  withJoinedSpace,
  withoutJoinedSpace,
  type JoinedSpacePreference,
} from "../src/lib/xmpp/joined-spaces";
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

function withXml(run: () => Promise<void>): Promise<void> {
  return withFakeDomParser(() => withFakeXmlDocument(run));
}

function storedXml(spaces: string): string {
  return `<iq type="result"><query xmlns="jabber:iq:private"><spaces xmlns="urn:waddle:joined-spaces:0">${spaces}</spaces></query></iq>`;
}

describe("joined-space preferences", () => {
  test("entries are keyed by service and node together", () => {
    expect(joinedSpaceKey({ serviceJid: "spaces.a.test", node: "main" }))
      .not.toBe(joinedSpaceKey({ serviceJid: "spaces.b.test", node: "main" }));
    expect(joinedSpaceKey({ serviceJid: "Spaces.A.test.", node: "main" }))
      .toBe(joinedSpaceKey({ serviceJid: "spaces.a.test", node: "main" }));
  });

  test("a stored list is read without duplicates and without anything outside the schema", async () => {
    await withFakeDomParser(async () => {
      const doc = new DOMParser().parseFromString(storedXml(
        '<space service="spaces.a.test" node="main" name="Main" follow="true" role="owner" affiliation="owner"/>'
        + '<space service="SPACES.A.test" node="main" name="Duplicate"/>'
        + '<space service="spaces.b.test" node="main"/>'
        + '<space service="" node="orphan"/>',
      ), "text/xml");
      const preferences = parseJoinedSpaces(doc);
      expect(preferences).toEqual([
        { serviceJid: "spaces.a.test", node: "main", name: "Main", follow: true },
        { serviceJid: "spaces.b.test", node: "main", follow: false },
      ]);
      // A preference is never a standing: no role or affiliation is read.
      expect(Object.keys(preferences[0]!).sort()).toEqual(["follow", "name", "node", "serviceJid"]);
    });
  });

  test("adding updates an entry in place and removing drops only that Space", () => {
    const list: JoinedSpacePreference[] = [
      { serviceJid: "spaces.a.test", node: "main", follow: true },
      { serviceJid: "spaces.b.test", node: "main", follow: true },
    ];
    const updated = withJoinedSpace(list, { serviceJid: "spaces.a.test", node: "main", follow: false });
    expect(updated).toEqual([
      { serviceJid: "spaces.a.test", node: "main", follow: false },
      { serviceJid: "spaces.b.test", node: "main", follow: true },
    ]);
    expect(withoutJoinedSpace(updated, { serviceJid: "spaces.b.test", node: "main" })).toEqual([
      { serviceJid: "spaces.a.test", node: "main", follow: false },
    ]);
  });

  test("the list is stored in private XML storage under its own namespace", async () => {
    await withXml(async () => {
      expect(buildLoadJoinedSpacesIq("l1")).toContain(
        '<query xmlns="jabber:iq:private"><spaces xmlns="urn:waddle:joined-spaces:0"></spaces></query>',
      );
      const xml = buildSaveJoinedSpacesIq("s1", [{ serviceJid: "spaces.a.test", node: "main", name: "Main", follow: true }]);
      expect(xml).toContain('type="set"');
      expect(xml).toContain('<space service="spaces.a.test" node="main" name="Main" follow="true">');
    });
  });

  test("the store reads and writes the list on the user's own account", async () => {
    await withXml(async () => {
      const sent: string[] = [];
      const xmpp = {
        async send_raw_iq(xml: string) {
          sent.push(xml);
          return xml.includes('type="get"')
            ? storedXml('<space service="spaces.a.test" node="main" follow="1"/>')
            : '<iq type="result"/>';
        },
      };
      const store = privateXmlJoinedSpaceStore(xmpp, "alice@home.test");
      expect(await store.load()).toEqual([{ serviceJid: "spaces.a.test", node: "main", follow: true }]);
      await store.save([]);
      expect(sent).toHaveLength(2);
      // Addressed to the account itself: no `to`, so the home server answers.
      expect(sent.every((xml) => !xml.includes(" to="))).toBe(true);
    });
  });
});
