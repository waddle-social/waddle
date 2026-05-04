/**
 * Stanza/jxt registration for the generic Waddle extension PubSub item envelope.
 *
 * Every Waddle extension publishes its PubSub state items wrapped in a single
 * `<extension-item xmlns="urn:waddle:extension:1">` element with a fixed
 * vocabulary of generic UI primitives (title/subtitle/link/description/
 * field/option/action). The host renders these uniformly regardless of which
 * extension produced them, so this file registers exactly one parser — there
 * are no per-extension entries.
 */
import type { DefinitionOptions, FieldDefinition, JSONElement } from "stanza/jxt";
import { pubsubItemContentAliases, staticValue, text } from "stanza/jxt";

const EXTENSION_FRAMEWORK_NAMESPACE = "urn:waddle:extension:1";
const EXTENSION_ITEM_ELEMENT = "extension-item";

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

const extensionItemDefinition: DefinitionOptions = {
  aliases: pubsubItemContentAliases(),
  element: EXTENSION_ITEM_ELEMENT,
  fields: {
    name: staticValue(EXTENSION_ITEM_ELEMENT),
    namespace: staticValue(EXTENSION_FRAMEWORK_NAMESPACE),
    itemType: staticValue(`${EXTENSION_FRAMEWORK_NAMESPACE}:${EXTENSION_ITEM_ELEMENT}`),
    attributes: rawAttributes(EXTENSION_FRAMEWORK_NAMESPACE),
    children: rawChildElements(),
    text: text(),
  },
  namespace: EXTENSION_FRAMEWORK_NAMESPACE,
};

const definitions: DefinitionOptions[] = [extensionItemDefinition];

export default definitions;
