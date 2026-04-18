/**
 * XEP-0428: Fallback Indication.
 *
 *   <fallback xmlns="urn:xmpp:fallback:0" for="urn:xmpp:reply:0">
 *     <body start="0" end="18"/>
 *   </fallback>
 *
 * A message MAY carry several `<fallback/>` payloads (one per feature
 * namespace). The inner `<body/>` is optional; when absent the entire
 * message body is fallback.
 */
import type { DefinitionOptions, FieldDefinition } from "stanza/jxt";
import { attribute } from "stanza/jxt";
import XMLElement from "stanza/jxt/Element";

const NS_FALLBACK = "urn:xmpp:fallback:0";

export interface WaddleFallbackBodyRange {
  start?: number;
  end?: number;
}

export interface WaddleFallback {
  for: string;
  body?: WaddleFallbackBodyRange;
}

function parseUtf16Offset(raw: string | undefined): number | undefined {
  if (raw === undefined || raw === "") return undefined;
  const parsed = parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed < 0) return undefined;
  return parsed;
}

const bodyField: FieldDefinition<WaddleFallbackBodyRange> = {
  importer(xml: XMLElement): WaddleFallbackBodyRange | undefined {
    const child = xml.getChild("body");
    if (!child) return undefined;
    const start = parseUtf16Offset(child.getAttribute("start"));
    const end = parseUtf16Offset(child.getAttribute("end"));
    const out: WaddleFallbackBodyRange = {};
    if (start !== undefined) out.start = start;
    if (end !== undefined) out.end = end;
    return out;
  },
  exporter(xml: XMLElement, value: WaddleFallbackBodyRange | undefined): void {
    if (value === undefined) return;
    const attrs: Record<string, string> = {};
    if (value.start !== undefined) attrs.start = String(value.start);
    if (value.end !== undefined) attrs.end = String(value.end);
    xml.appendChild(new XMLElement("body", attrs));
  },
};

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.fallbacks", multiple: true }],
    element: "fallback",
    fields: {
      for: attribute("for"),
      body: bodyField,
    },
    namespace: NS_FALLBACK,
  },
];

export default definitions;
