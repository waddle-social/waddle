/**
 * Shared XEP-0030 disco fixture + fake-DOM helpers.
 *
 * Both `discovery-pin-permission.test.ts` and `discovery-resilience.test.ts`
 * exercise `chat/src/lib/xmpp/discovery.ts`, which parses disco IQ
 * responses via the browser `DOMParser`. bun:test runs without a DOM, so
 * each suite needs a minimal XML parser substitute. Centralised here so
 * the two suites can't drift on what shape of XML they emit / parse.
 */

type DiscoIdentity = { category: string; type: string; name?: string };

export function discoItemsXml(items: Array<{ name?: string; node?: string; jid?: string }>): string {
  return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#items">${items.map((item) =>
    `<item${item.jid ? ` jid="${item.jid}"` : ""}${item.name ? ` name="${item.name}"` : ""}${item.node ? ` node="${item.node}"` : ""}/>`
  ).join("")}</query></iq>`;
}

export function discoInfoXml(info: { features?: string[]; identities?: DiscoIdentity[] } = {}): string {
  return `<iq type="result"><query xmlns="http://jabber.org/protocol/disco#info">${
    (info.identities ?? []).map((identity) =>
      `<identity category="${identity.category}" type="${identity.type}"${identity.name ? ` name="${identity.name}"` : ""}/>`
    ).join("")
  }${
    (info.features ?? []).map((feature) => `<feature var="${feature}"/>`).join("")
  }</query></iq>`;
}

export function pubsubItemsXml(items: Array<{ id?: string; jid?: string; name?: string; autojoin?: boolean }>): string {
  return `<iq type="result"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items>${items.map((item) =>
    `<item${item.id ? ` id="${item.id}"` : ""}>${item.jid || item.name ? `<conference xmlns="urn:xmpp:bookmarks:1"${item.jid ? ` jid="${item.jid}"` : ""}${item.name ? ` name="${item.name}"` : ""}${item.autojoin === undefined ? "" : ` autojoin="${item.autojoin ? "true" : "false"}"`}/>` : ""}</item>`
  ).join("")}</items></pubsub></iq>`;
}

export async function withFakeDomParser(run: () => Promise<void>): Promise<void> {
  const original = globalThis.DOMParser;
  globalThis.DOMParser = FakeDOMParser as typeof DOMParser;
  try {
    await run();
  } finally {
    globalThis.DOMParser = original;
  }
}

export async function withFakeXmlDocument(run: () => Promise<void>): Promise<void> {
  const originalDocument = globalThis.document;
  const originalXmlSerializer = globalThis.XMLSerializer;
  globalThis.document = {
    implementation: {
      createDocument: (_namespace: string, rootName: string) => new FakeStructuredXmlDocument(rootName),
    },
  } as unknown as Document;
  globalThis.XMLSerializer = FakeXmlSerializer as typeof XMLSerializer;
  try {
    await run();
  } finally {
    globalThis.document = originalDocument;
    globalThis.XMLSerializer = originalXmlSerializer;
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
    attrs[match[1]] = decodeXmlAttribute(match[2]);
  }
  return attrs;
}

function decodeXmlAttribute(value: string): string {
  return value
    .replaceAll("&quot;", "\"")
    .replaceAll("&apos;", "'")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&amp;", "&");
}

class FakeStructuredXmlDocument {
  readonly documentElement: FakeStructuredXmlElement;

  constructor(rootName: string) {
    this.documentElement = new FakeStructuredXmlElement(rootName, null);
  }

  createElementNS(namespaceURI: string, name: string): FakeStructuredXmlElement {
    return new FakeStructuredXmlElement(name, namespaceURI);
  }

  createTextNode(value: string): FakeStructuredXmlText {
    return new FakeStructuredXmlText(value);
  }
}

class FakeStructuredXmlElement {
  readonly attrs: Record<string, string> = {};
  readonly children: Array<FakeStructuredXmlElement | FakeStructuredXmlText> = [];

  constructor(
    readonly localName: string,
    readonly namespaceURI: string | null,
  ) {}

  setAttribute(name: string, value: string): void {
    this.attrs[name] = value;
  }

  appendChild(
    child: FakeStructuredXmlElement | FakeStructuredXmlText,
  ): FakeStructuredXmlElement | FakeStructuredXmlText {
    this.children.push(child);
    return child;
  }
}

class FakeStructuredXmlText {
  constructor(readonly textContent: string) {}
}

class FakeXmlSerializer {
  serializeToString(node: FakeStructuredXmlElement): string {
    return serializeFakeXmlElement(node, null);
  }
}

function serializeFakeXmlElement(node: FakeStructuredXmlElement, parentNamespace: string | null): string {
  const namespaceAttr = node.namespaceURI && node.namespaceURI !== parentNamespace
    ? ` xmlns="${escapeTestXml(node.namespaceURI)}"`
    : "";
  const attrs = Object.entries(node.attrs)
    .map(([name, value]) => ` ${name}="${escapeTestXml(value)}"`)
    .join("");
  const children = node.children.map((child) =>
    child instanceof FakeStructuredXmlText
      ? escapeTestXml(child.textContent)
      : serializeFakeXmlElement(child, node.namespaceURI),
  ).join("");
  return `<${node.localName}${namespaceAttr}${attrs}>${children}</${node.localName}>`;
}

function escapeTestXml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}
