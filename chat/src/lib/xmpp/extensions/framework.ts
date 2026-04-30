/** Waddle unified extension framework envelope. */
import type { DefinitionOptions, FieldDefinition, JSONElement } from "stanza/jxt";
import { attribute, childText, text } from "stanza/jxt";

const NS_WADDLE_EXTENSION_1 = "urn:waddle:extension:1";

type RawXmlElement = {
  getNamespace: () => string;
  getName: () => string;
  attributes: Record<string, unknown>;
  children: unknown[];
};

function isRawXmlElement(value: unknown): value is RawXmlElement {
  return !!value
    && typeof value === "object"
    && typeof (value as RawXmlElement).getNamespace === "function"
    && typeof (value as RawXmlElement).getName === "function"
    && Array.isArray((value as RawXmlElement).children);
}

function rawElementJson(xml: RawXmlElement): JSONElement {
  const namespace = xml.getNamespace();
  const attributes: JSONElement["attributes"] = {};
  for (const [key, value] of Object.entries(xml.attributes)) {
    if (typeof value === "string") attributes[key] = value;
  }
  if (namespace && attributes.xmlns !== namespace) {
    attributes.xmlns = namespace;
  }
  const children: JSONElement["children"] = [];
  for (const child of xml.children) {
    if (typeof child === "string") children.push(child);
    else if (isRawXmlElement(child)) children.push(rawElementJson(child));
  }
  return {
    name: xml.getName(),
    attributes,
    children,
  };
}

function rawChildElements(): FieldDefinition<JSONElement[]> {
  return {
    importer(xml) {
      return xml.children.flatMap((child) => isRawXmlElement(child) ? [rawElementJson(child)] : []);
    },
    exporter() {
      return undefined;
    },
  };
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.waddleExtensions", multiple: false }],
    element: "extensions",
    fields: {
      version: attribute("version"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments", multiple: true }],
    element: "enrichment",
    fields: {
      id: attribute("id"),
      plugin: attribute("plugin"),
      capability: attribute("capability"),
      payloadNamespace: attribute("payload-ns"),
      surface: attribute("surface"),
      payloadSurface: attribute("payload-surface"),
      uiSurface: attribute("ui-surface"),
      created: attribute("created"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.source", multiple: false }],
    element: "source",
    fields: {
      stanzaId: attribute("stanza-id"),
      by: attribute("by"),
      bodyStart: attribute("body-start"),
      bodyEnd: attribute("body-end"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload", multiple: false }],
    element: "payload",
    fields: {
      elements: rawChildElements(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.views", multiple: true }],
    element: "view",
    fields: {
      id: attribute("id"),
      title: attribute("title"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.payload.views.textBlocks", multiple: true }],
    element: "text",
    fields: {
      style: attribute("style"),
      text: text(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.launches", multiple: true }],
    element: "launch",
    fields: {
      id: attribute("id"),
      plugin: attribute("plugin"),
      action: attribute("action"),
      commandNode: attribute("command-node"),
      token: attribute("token"),
      label: attribute("label"),
      expiresAt: attribute("expires-at"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.launches.context", multiple: false }],
    element: "context",
    fields: {
      waddleId: attribute("waddle-id"),
      room: attribute("room"),
      roomJid: attribute("room-jid"),
      stanzaId: attribute("stanza-id"),
      sourceStanzaId: attribute("source-stanza-id"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.enrichments.launches.payload", multiple: false }],
    element: "payload",
    fields: {
      elements: rawChildElements(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations", multiple: true }],
    element: "annotation",
    fields: {
      extension: attribute("extension"),
      id: attribute("id"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations.card", multiple: false }],
    element: "card",
    fields: {
      title: childText(null, "title"),
      summary: childText(null, "summary"),
      image: childText(null, "image"),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations.card.fields", multiple: true }],
    element: "field",
    fields: {
      name: attribute("name"),
      value: text(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
  {
    aliases: [{ path: "message.waddleExtensions.annotations.card.actions", multiple: true }],
    element: "action",
    fields: {
      route: attribute("route"),
      label: text(),
    },
    namespace: NS_WADDLE_EXTENSION_1,
  },
];

export default definitions;
