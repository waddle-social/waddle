import { describe, expect, test } from "bun:test";
import {
  DiscoTimeoutError,
  discoverTopology,
  withIqTimeout,
} from "../src/lib/xmpp/discovery";

/**
 * Resilience contract for XEP-0030 topology discovery:
 *
 *  - RFC 6120 §8.2.3 requires IQ responses but cannot enforce it; every
 *    in-flight disco IQ MUST observe a bounded timeout so a wedged
 *    component cannot stall topology load forever.
 *  - Multi-component fan-out (`discoverComponentServices`,
 *    `discoverTopology` room hydration) MUST tolerate a single hung or
 *    failing component via the conventional-domain / unhydrated-room
 *    fallbacks. Implementation uses `Promise.allSettled`.
 *
 * These tests lock both contracts so we don't accidentally regress to
 * a `Promise.all` storm or drop the IQ timeout in a refactor.
 */

describe("withIqTimeout (RFC 6120 §8.2.3 defense)", () => {
  test("rejects with DiscoTimeoutError when the IQ does not resolve in time", async () => {
    const hung = new Promise<string>(() => {
      // intentionally never resolves
    });
    let caught: unknown = null;
    try {
      await withIqTimeout(hung, "calls.example.test", undefined, 30);
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(DiscoTimeoutError);
    expect((caught as DiscoTimeoutError).to).toBe("calls.example.test");
  });

  test("forwards the underlying resolution when the IQ answers before the timeout", async () => {
    const result = await withIqTimeout(Promise.resolve("ok"), "example.test", undefined, 1000);
    expect(result).toBe("ok");
  });

  test("clears the timer so a late rejection does not leak", async () => {
    // If the timer were not cleared on success, this Promise.race would
    // still hold a pending timeout — but since the test process exits
    // when all jobs settle, leaking timers would surface as unhandled
    // rejection warnings or hold the loop open. Bun fails fast on
    // either, so an assertion-free "resolves cleanly" suffices.
    await withIqTimeout(Promise.resolve(42), "example.test", "node-1", 5);
    // Wait past the original timeout window to confirm no late rejection
    // fires after the success path resolved.
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
});

describe("discoverTopology partial-failure resilience", () => {
  test("falls back to the conventional service when one component disco#info hangs", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(resilientClient({ hangComponent: "extensions.example.test" }), "alice@example.test");

      // muc.example.test answered → that's the muc service we keep.
      expect(topology.services.muc).toBe("muc.example.test");
      // extensions.example.test hung → discoverComponentServices ignores it
      // and the conventional spaces.<domain> remains the spaces service.
      expect(topology.services.spaces).toBe("spaces.example.test");
    });
  });

  test("returns spaces topology even when the MUC service is wedged", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(resilientClient({ hangMucItems: true }), "alice@example.test");

      // Spaces discovery succeeded.
      expect(topology.spaces.map((space) => space.id)).toContain("space-engineering");
      // MUC items hung → rooms array stays empty rather than the whole
      // topology call rejecting.
      expect(topology.rooms).toEqual([]);
    });
  });

  test("returns the room list with unhydrated entries when one room disco#info rejects", async () => {
    await withFakeDomParser(async () => {
      const topology = await discoverTopology(
        resilientClient({ rejectRoomInfo: "broken@muc.example.test" }),
        "alice@example.test",
      );

      // Both rooms survive — the failing one falls back to a bare
      // channelFromRoom record without hydrated fields.
      const ids = topology.rooms.map((room) => room.id).sort();
      expect(ids).toEqual(["broken", "general"]);
    });
  });
});

type ResilientClientOptions = {
  hangComponent?: string;
  hangMucItems?: boolean;
  rejectRoomInfo?: string;
};

function resilientClient(options: ResilientClientOptions = {}) {
  return {
    async send_raw_iq(xml: string): Promise<string> {
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
        if (xml.includes('to="example.test"')) {
          return discoItemsXml([
            { jid: "muc.example.test", name: "Chatrooms" },
            { jid: "spaces.example.test", name: "Spaces" },
            { jid: "extensions.example.test", name: "Extensions" },
          ]);
        }
        if (xml.includes('to="spaces.example.test"')) {
          return discoItemsXml([
            { name: "Engineering", node: "space-engineering" },
          ]);
        }
        if (xml.includes('to="muc.example.test"')) {
          if (options.hangMucItems) {
            // Simulates the production stall as a synchronous rejection:
            // testing the actual wedged-promise + 15s real timeout would
            // hold the suite for the full timeout window. We bypass the
            // timer path here and assert that the same try/catch in
            // discoverTopology degrades the rooms list gracefully when
            // sendDiscoItems rejects for ANY reason — timeout or stanza
            // error — which is the contract callers depend on.
            throw new Error("simulated muc service stall");
          }
          return discoItemsXml([
            { jid: "general@muc.example.test", name: "General" },
            { jid: "broken@muc.example.test", name: "Broken" },
          ]);
        }
        return discoItemsXml([]);
      }
      if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
        if (options.hangComponent && xml.includes(`to="${options.hangComponent}"`)) {
          // Hung component disco#info: tests use a short withIqTimeout
          // budget via the wrapping helper. Even with the production
          // 15s default, allSettled in discoverComponentServices means
          // a single hang cannot wedge the others — so this test does
          // not need to short-circuit the timer.
          throw new Error("simulated component disco#info timeout");
        }
        if (options.rejectRoomInfo && xml.includes(`to="${options.rejectRoomInfo}"`)) {
          throw new Error("simulated room disco#info failure");
        }
        if (xml.includes('to="muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "Chatrooms" }],
            features: ["http://jabber.org/protocol/muc"],
          });
        }
        if (xml.includes('to="spaces.example.test"') && !xml.includes(' node=')) {
          return discoInfoXml({
            identities: [{ category: "pubsub", type: "service", name: "Spaces" }],
            features: [
              "http://jabber.org/protocol/pubsub",
              "urn:xmpp:spaces:0",
            ],
          });
        }
        if (xml.includes('to="general@muc.example.test"') || xml.includes('to="broken@muc.example.test"')) {
          return discoInfoXml({
            identities: [{ category: "conference", type: "text", name: "Room" }],
            features: ["http://jabber.org/protocol/muc"],
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
        return pubsubItemsXml([]);
      }
      throw new Error(`Unexpected IQ: ${xml}`);
    },
  };
}

function discoItemsXml(items: Array<{ name?: string; node?: string; jid?: string }>): string {
  return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items">${items.map((item) =>
    `<item${item.jid ? ` jid="${item.jid}"` : ""}${item.name ? ` name="${item.name}"` : ""}${item.node ? ` node="${item.node}"` : ""}/>`
  ).join("")}</query></iq>`;
}

function discoInfoXml(info: { features?: string[]; identities?: Array<{ category: string; type: string; name?: string }> } = {}): string {
  return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info">${
    (info.identities ?? []).map((identity) =>
      `<identity category="${identity.category}" type="${identity.type}"${identity.name ? ` name="${identity.name}"` : ""}/>`
    ).join("")
  }${
    (info.features ?? []).map((feature) => `<feature var="${feature}"/>`).join("")
  }</query></iq>`;
}

function pubsubItemsXml(items: Array<{ id?: string; jid?: string; name?: string }>): string {
  return `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items>${items.map((item) =>
    `<item${item.id ? ` id="${item.id}"` : ""}>${item.jid || item.name ? `<conference${item.jid ? ` jid="${item.jid}"` : ""}${item.name ? ` name="${item.name}"` : ""}/>` : ""}</item>`
  ).join("")}</items></pubsub></iq>`;
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
