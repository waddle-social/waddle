import { describe, expect, test } from "bun:test";
import { discoverTopology } from "../src/lib/xmpp/discovery";

/**
 * Discovery extracts the XEP-0402 `autojoin` attribute (and optional
 * `<nick/>`) from `<conference xmlns='urn:xmpp:bookmarks:1'>` items
 * published into a Space's PubSub node. The auto-join fan-out keyed
 * off the resulting topology is what makes XEP-0272 Muji presence —
 * and therefore the per-room call indicator — visible across the
 * whole sidebar after a refresh.
 *
 * Coverage:
 * - `autojoin='true'` → `room.autojoin === true`
 * - `autojoin='false'` → `room.autojoin === false`
 * - Attribute absent → defaults to `true` (waddle's de-facto: a room
 *   surfaced via MUC service items should be auto-joined unless the
 *   bookmark explicitly opts out).
 * - A room with no Space bookmark at all → defaults to `true`.
 * - `<nick/>` child → surfaced as `room.bookmarkNick`.
 */
describe("discoverTopology autojoin parsing", () => {
  test("autojoin=true bookmark surfaces room.autojoin=true", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          spaceBookmarks: { "space-engineering": [{ jid: "general@muc.example.test", name: "General", autojoin: true }] },
          mucRooms: [{ jid: "general@muc.example.test", name: "General" }],
        }),
        "alice@example.test",
      );

      const general = topology.rooms.find((r) => r.jid === "general@muc.example.test");
      expect(general?.autojoin).toBe(true);
      expect(general?.spaceId).toBe("space-engineering");
    });
  });

  test("autojoin=false bookmark surfaces room.autojoin=false", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          spaceBookmarks: { "space-engineering": [{ jid: "muted@muc.example.test", name: "Muted", autojoin: false }] },
          mucRooms: [{ jid: "muted@muc.example.test", name: "Muted" }],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.find((r) => r.jid === "muted@muc.example.test")?.autojoin).toBe(false);
    });
  });

  test("autojoin attribute absent defaults to true", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          spaceBookmarks: { "space-engineering": [{ jid: "shared@muc.example.test", name: "Shared" }] },
          mucRooms: [{ jid: "shared@muc.example.test", name: "Shared" }],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.find((r) => r.jid === "shared@muc.example.test")?.autojoin).toBe(true);
    });
  });

  test("room with no Space bookmark at all still defaults to autojoin=true", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          spaceBookmarks: {},
          mucRooms: [{ jid: "orphan@muc.example.test", name: "Orphan" }],
        }),
        "alice@example.test",
      );

      const orphan = topology.rooms.find((r) => r.jid === "orphan@muc.example.test");
      expect(orphan?.autojoin).toBe(true);
      expect(orphan?.spaceId).toBeUndefined();
    });
  });

  test("bookmark <nick/> child propagates as room.bookmarkNick", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        topologyClient({
          spaceBookmarks: { "space-engineering": [{ jid: "nicked@muc.example.test", name: "Nicked", autojoin: true, nick: "alias42" }] },
          mucRooms: [{ jid: "nicked@muc.example.test", name: "Nicked" }],
        }),
        "alice@example.test",
      );

      expect(topology.rooms.find((r) => r.jid === "nicked@muc.example.test")?.bookmarkNick).toBe("alias42");
    });
  });
});

type BookmarkItem = { jid?: string; name?: string; autojoin?: boolean; nick?: string };

type TopologyClientOptions = {
  spaceBookmarks: Record<string, BookmarkItem[]>;
  mucRooms: Array<{ jid: string; name?: string }>;
};

function topologyClient(options: TopologyClientOptions) {
  const spaceNodes = Object.keys(options.spaceBookmarks);
  return {
    async send_raw_iq(xml: string): Promise<string> {
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          return discoItemsXml([
            { jid: "spaces.example.test", name: "Spaces" },
            { jid: "muc.example.test", name: "Chatrooms" },
          ]);
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoItemsXml(spaceNodes.map((node) => ({ name: titleCase(node), node })));
        }
        if (xml.includes('to="muc.example.test"')) {
          return discoItemsXml(options.mucRooms);
        }
        return discoItemsXml([]);
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (xml.includes('to="muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "Chatrooms" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service", name: "Spaces" }],
            features: ["http://jabber.org/protocol/pubsub"],
          });
        }
        if (xml.includes('to="example.test"')) {
          return discoInfoXml({
            identities: [{ category: "server", type: "im", name: "Waddle" }],
            features: [],
          });
        }
        return discoInfoXml();
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/pubsub"')) {
        const nodeMatch = /node="([^"]+)"/.exec(xml);
        const node = nodeMatch?.[1];
        const bookmarks = node ? options.spaceBookmarks[node] ?? [] : [];
        return pubsubItemsXml(bookmarks.map((b) => ({ id: b.jid, ...b })));
      }
      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
}

function titleCase(node: string): string {
  return node.replace(/^space-/, "").replace(/^./, (c) => c.toUpperCase());
}

function discoItemsXml(items: Array<{ name?: string; node?: string; jid?: string }>): string {
  return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items">${items.map((item) =>
    `<item${item.jid ? ` jid="${item.jid}"` : ""}${item.name ? ` name="${item.name}"` : ""}${item.node ? ` node="${item.node}"` : ""}/>`,
  ).join("")}</query></iq>`;
}

function discoInfoXml(info: { features?: string[]; identities?: Array<{ category: string; type: string; name?: string }> } = {}): string {
  return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info">${
    (info.identities ?? []).map((identity) =>
      `<identity category="${identity.category}" type="${identity.type}"${identity.name ? ` name="${identity.name}"` : ""}/>`,
    ).join("")
  }${
    (info.features ?? []).map((feature) => `<feature var="${feature}"/>`).join("")
  }</query></iq>`;
}

function pubsubItemsXml(items: Array<{ id?: string; jid?: string; name?: string; autojoin?: boolean; nick?: string }>): string {
  return `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items>${items.map((item) => {
    if (!(item.jid || item.name)) return `<item${item.id ? ` id="${item.id}"` : ""}/>`;
    const autojoinAttr = item.autojoin === undefined ? "" : ` autojoin="${item.autojoin ? "true" : "false"}"`;
    const conferenceChildren = item.nick ? `<nick xmlns="urn:xmpp:bookmarks:1">${item.nick}</nick>` : "";
    const conferenceTag = `<conference xmlns="urn:xmpp:bookmarks:1"${item.jid ? ` jid="${item.jid}"` : ""}${item.name ? ` name="${item.name}"` : ""}${autojoinAttr}${conferenceChildren ? `>${conferenceChildren}</conference>` : "/>"}`;
    return `<item${item.id ? ` id="${item.id}"` : ""}>${conferenceTag}</item>`;
  }).join("")}</items></pubsub></iq>`;
}

async function withFakeDomParser(run: () => Promise<void>): Promise<void> {
  const original = globalThis.DOMParser;
  globalThis.DOMParser = FakeDOMParser as typeof DOMParser;
  try {
    await run();
  } finally {
    globalThis.DOMParser = original;
  }
}

class FakeDOMParser {
  parseFromString(xml: string): Document {
    return parseTestXml(xml) as unknown as Document;
  }
}

class FakeXmlElement {
  readonly children: FakeXmlElement[] = [];
  text = "";
  parentNode: FakeXmlElement | null = null;

  constructor(
    readonly localName: string,
    readonly namespaceURI: string | null,
    private readonly attrs: Record<string, string>,
  ) {}

  get textContent(): string {
    return `${this.text}${this.children.map((child) => child.textContent ?? "").join("")}`;
  }

  getAttribute(name: string): string | null {
    return this.attrs[name] ?? null;
  }

  querySelector(localName: string): FakeXmlElement | null {
    return this.getElementsByTagName(localName)[0] ?? null;
  }

  getElementsByTagNameNS(namespace: string, localName: string): FakeXmlElement[] {
    return this.descendants().filter((node) => node.namespaceURI === namespace && node.localName === localName);
  }

  getElementsByTagName(localName: string): FakeXmlElement[] {
    return this.descendants().filter((node) => node.localName === localName);
  }

  private descendants(): FakeXmlElement[] {
    return this.children.flatMap((child) => [child, ...child.descendants()]);
  }
}

function parseTestXml(xml: string): FakeXmlElement {
  const root = new FakeXmlElement("#document", null, {});
  const stack: FakeXmlElement[] = [root];
  const tokenPattern = /<[^>]+>|[^<]+/g;
  for (const token of xml.match(tokenPattern) ?? []) {
    if (token.startsWith("</")) {
      stack.pop();
      continue;
    }
    if (token.startsWith("<")) {
      const selfClosing = token.endsWith("/>");
      const body = token.slice(1, selfClosing ? -2 : -1).trim();
      const [qualifiedName = ""] = body.split(/\s+/, 1);
      const attrs = attributesFromTag(body);
      const parent = stack[stack.length - 1];
      const node = new FakeXmlElement(
        qualifiedName.includes(":") ? qualifiedName.split(":").pop()! : qualifiedName,
        attrs.xmlns ?? parent.namespaceURI,
        attrs,
      );
      node.parentNode = parent;
      parent.children.push(node);
      if (!selfClosing) stack.push(node);
      continue;
    }
    stack[stack.length - 1].text += token;
  }
  return root;
}

function attributesFromTag(tagBody: string): Record<string, string> {
  const attrs: Record<string, string> = {};
  for (const match of tagBody.matchAll(/([A-Za-z0-9:_-]+)="([^"]*)"/g)) {
    attrs[match[1]] = match[2];
  }
  return attrs;
}
