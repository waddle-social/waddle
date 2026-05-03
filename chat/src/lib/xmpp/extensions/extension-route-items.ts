/** PubSub item payloads retained by Waddle extension routes. */
import type { DefinitionOptions, FieldDefinition, JSONElement } from "stanza/jxt";
import { pubsubItemContentAliases, staticValue, text } from "stanza/jxt";

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

function rawAttributes(namespace: string): FieldDefinition<Record<string, string>> {
  return {
    importer(xml) {
      const attributes: Record<string, string> = {};
      for (const [key, value] of Object.entries(xml.attributes)) {
        if (typeof value === "string") attributes[key] = value;
      }
      attributes.xmlns = namespace;
      return attributes;
    },
    exporter() {
      return undefined;
    },
  };
}

function rawElementJson(xml: RawXmlElement): JSONElement {
  const namespace = xml.getNamespace();
  const attributes: JSONElement["attributes"] = {};
  for (const [key, value] of Object.entries(xml.attributes)) {
    if (typeof value === "string") attributes[key] = value;
  }
  if (namespace) attributes.xmlns = namespace;
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

function extensionPayload(namespace: string, root: string): DefinitionOptions {
  return {
    aliases: pubsubItemContentAliases(),
    element: root,
    fields: {
      name: staticValue(root),
      namespace: staticValue(namespace),
      itemType: staticValue(`${namespace}:${root}`),
      attributes: rawAttributes(namespace),
      children: rawChildElements(),
      text: text(),
    },
    namespace,
  };
}

const definitions: DefinitionOptions[] = [
  extensionPayload("urn:waddle:link-board:1", "link"),
  extensionPayload("urn:waddle:decision-polls:1", "poll"),
  extensionPayload("urn:waddle:decision-polls:1", "results"),
  extensionPayload("urn:waddle:decision-polls:1", "vote"),
];

export default definitions;
