export type XmppResumeStanzaKind = "message" | "presence" | "iq";

type XmppResumeXmlName = {
  namespace: string;
  localName: string;
};

export type XmppResumeXmlAttribute = {
  name: XmppResumeXmlName;
  value: string;
};

export type XmppResumeXmlToken =
  | { kind: "start"; name: XmppResumeXmlName; attributes: XmppResumeXmlAttribute[] }
  | { kind: "text"; value: string }
  | { kind: "end" };

export type XmppResumeStanza = {
  stanzaKind: XmppResumeStanzaKind;
  tokens: XmppResumeXmlToken[];
};

export type XmppResumeEntry = {
  stanza: XmppResumeStanza;
  sentAtEpochMs: number;
};

export type PersistedSmResumeState = {
  previd: string;
  inboundH: number;
  outboundH: number;
  maxResumeSeconds?: number;
  unhandledOutboundEntries: XmppResumeEntry[];
  resource?: string;
};

const RESUME_ENTRY_LIMIT = 4_096;
const RESUME_XML_TOKEN_LIMIT = 16_384;
const RESUME_XML_DEPTH_LIMIT = 64;
const RESUME_XML_ATTRIBUTE_LIMIT = 16_384;
const RESUME_XML_BYTE_LIMIT = 1024 * 1024;
const JS_DATE_LIMIT_MS = 8_640_000_000_000_000;
const utf8Encoder = new TextEncoder();
// XML 1.0 (Fifth Edition) NameStartChar / NameChar with `:` deliberately
// excluded, yielding the exact NCName production used by Namespaces in XML.
const XML_NCNAME = /^[_A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u02FF\u0370-\u037D\u037F-\u1FFF\u200C-\u200D\u2070-\u218F\u2C00-\u2FEF\u3001-\uD7FF\uF900-\uFDCF\uFDF0-\uFFFD\u{10000}-\u{EFFFF}][._\-A-Za-z0-9\u00B7\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u037D\u037F-\u1FFF\u200C-\u200D\u203F-\u2040\u2070-\u218F\u2C00-\u2FEF\u3001-\uD7FF\uF900-\uFDCF\uFDF0-\uFFFD\u0300-\u036F\u{10000}-\u{EFFFF}]*$/u;

type ResumeDecodeBudget = {
  tokenCount: number;
  attributeCount: number;
  utf8Bytes: number;
};

function corruptSmState(detail: string): never {
  throw new DOMException(
    `Corrupt XEP-0198 resume state: ${detail}`,
    "DataError",
  );
}

function exactObject(
  value: unknown,
  keys: readonly string[],
  detail: string,
): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return corruptSmState(`${detail} is not an object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    return corruptSmState(`${detail} has a custom prototype`);
  }
  const record = value as Record<string, unknown>;
  const allowed = new Set(keys);
  for (const key of Object.keys(record)) {
    if (!allowed.has(key)) {
      corruptSmState(`${detail} contains unknown field ${key}`);
    }
  }
  return record;
}

function isXml10String(value: string): boolean {
  for (const character of value) {
    const codePoint = character.codePointAt(0)!;
    const allowed = codePoint === 0x9
      || codePoint === 0xA
      || codePoint === 0xD
      || (codePoint >= 0x20 && codePoint <= 0xD7FF)
      || (codePoint >= 0xE000 && codePoint <= 0xFFFD)
      || (codePoint >= 0x10000 && codePoint <= 0x10FFFF);
    if (!allowed) return false;
  }
  return true;
}

function xmlString(value: unknown, detail: string): string {
  if (typeof value !== "string" || !isXml10String(value)) {
    return corruptSmState(`${detail} is not valid XML 1.0 text`);
  }
  return value;
}

function boundedString(
  value: unknown,
  budget: ResumeDecodeBudget,
  detail: string,
): string {
  const validated = xmlString(value, detail);
  budget.utf8Bytes += utf8Encoder.encode(validated).byteLength;
  if (budget.utf8Bytes > RESUME_XML_BYTE_LIMIT) {
    corruptSmState("UTF-8 payload exceeds the durable limit");
  }
  return validated;
}

function decodeU32(value: unknown, detail: string): number {
  if (
    typeof value !== "number"
    || !Number.isInteger(value)
    || value < 0
    || value > 0xFFFF_FFFF
  ) {
    return corruptSmState(`${detail} is not a u32`);
  }
  return value;
}

function decodeXmlName(
  value: unknown,
  budget: ResumeDecodeBudget,
  detail: string,
): XmppResumeXmlName {
  const record = exactObject(value, ["namespace", "localName"], detail);
  const namespace = boundedString(record.namespace, budget, `${detail}.namespace`);
  const localName = boundedString(record.localName, budget, `${detail}.localName`);
  if (!XML_NCNAME.test(localName)) {
    corruptSmState(`${detail}.localName is not an XML NCName`);
  }
  return { namespace, localName };
}

function decodeStanza(
  value: unknown,
  budget: ResumeDecodeBudget,
  detail: string,
): XmppResumeStanza {
  const record = exactObject(value, ["stanzaKind", "tokens"], detail);
  const stanzaKind = record.stanzaKind;
  if (stanzaKind !== "message" && stanzaKind !== "presence" && stanzaKind !== "iq") {
    return corruptSmState(`${detail}.stanzaKind is invalid`);
  }
  if (!Array.isArray(record.tokens) || record.tokens.length === 0) {
    return corruptSmState(`${detail}.tokens is not a non-empty array`);
  }
  budget.tokenCount += record.tokens.length;
  if (budget.tokenCount > RESUME_XML_TOKEN_LIMIT) {
    corruptSmState("XML token count exceeds the durable limit");
  }

  let depth = 0;
  let rootSeen = false;
  const tokens: XmppResumeXmlToken[] = [];
  record.tokens.forEach((rawToken, tokenIndex) => {
    const tokenDetail = `${detail}.tokens[${tokenIndex}]`;
    const preliminary = exactObject(
      rawToken,
      ["kind", "name", "attributes", "value"],
      tokenDetail,
    );
    if (preliminary.kind === "start") {
      const token = exactObject(rawToken, ["kind", "name", "attributes"], tokenDetail);
      const name = decodeXmlName(token.name, budget, `${tokenDetail}.name`);
      if (!Array.isArray(token.attributes)) {
        corruptSmState(`${tokenDetail}.attributes is not an array`);
      }
      if (depth === 0) {
        if (rootSeen) corruptSmState(`${detail} contains more than one root`);
        rootSeen = true;
        if (name.namespace !== "jabber:client" || name.localName !== stanzaKind) {
          corruptSmState(`${detail} root does not match stanzaKind`);
        }
      }
      depth += 1;
      if (depth > RESUME_XML_DEPTH_LIMIT) {
        corruptSmState(`${detail} exceeds the XML depth limit`);
      }
      budget.attributeCount += token.attributes.length;
      if (budget.attributeCount > RESUME_XML_ATTRIBUTE_LIMIT) {
        corruptSmState("XML attribute count exceeds the durable limit");
      }
      const expandedNames = new Set<string>();
      const attributes = token.attributes.map((rawAttribute, attributeIndex) => {
        const attributeDetail = `${tokenDetail}.attributes[${attributeIndex}]`;
        const attribute = exactObject(
          rawAttribute,
          ["name", "value"],
          attributeDetail,
        );
        const attributeName = decodeXmlName(
          attribute.name,
          budget,
          `${attributeDetail}.name`,
        );
        const expandedName = `${attributeName.namespace}\0${attributeName.localName}`;
        if (expandedNames.has(expandedName)) {
          corruptSmState(`${tokenDetail} contains duplicate expanded attributes`);
        }
        expandedNames.add(expandedName);
        return {
          name: attributeName,
          value: boundedString(attribute.value, budget, `${attributeDetail}.value`),
        };
      });
      tokens.push({ kind: "start", name, attributes });
      return;
    }
    if (preliminary.kind === "text") {
      const token = exactObject(rawToken, ["kind", "value"], tokenDetail);
      if (depth === 0) corruptSmState(`${tokenDetail} is outside the root`);
      tokens.push({
        kind: "text",
        value: boundedString(token.value, budget, `${tokenDetail}.value`),
      });
      return;
    }
    if (preliminary.kind === "end") {
      exactObject(rawToken, ["kind"], tokenDetail);
      if (depth === 0) corruptSmState(`${tokenDetail} underflows XML depth`);
      depth -= 1;
      tokens.push({ kind: "end" });
      return;
    }
    corruptSmState(`${tokenDetail}.kind is invalid`);
  });
  if (!rootSeen || depth !== 0) {
    corruptSmState(`${detail} has an unbalanced XML root`);
  }
  return { stanzaKind, tokens };
}

/**
 * The one durable XEP-0198 semantic decoder. It both validates and rebuilds
 * the complete typed state so callers never retain an untrusted object graph.
 */
export function decodePersistedSmResumeState(
  value: unknown,
  detail = "state",
): PersistedSmResumeState {
  const budget: ResumeDecodeBudget = {
    tokenCount: 0,
    attributeCount: 0,
    utf8Bytes: 0,
  };
  const record = exactObject(
    value,
    [
      "previd",
      "inboundH",
      "outboundH",
      "maxResumeSeconds",
      "unhandledOutboundEntries",
      "resource",
    ],
    detail,
  );
  const previd = xmlString(record.previd, `${detail}.previd`);
  if (previd.length === 0) corruptSmState(`${detail}.previd is empty`);
  const maxResumeSeconds = record.maxResumeSeconds === undefined
    ? undefined
    : decodeU32(record.maxResumeSeconds, `${detail}.maxResumeSeconds`);
  if (maxResumeSeconds === 0) {
    corruptSmState(`${detail}.maxResumeSeconds is zero`);
  }
  const resource = record.resource === undefined
    ? undefined
    : xmlString(record.resource, `${detail}.resource`);
  if (
    !Array.isArray(record.unhandledOutboundEntries)
    || record.unhandledOutboundEntries.length > RESUME_ENTRY_LIMIT
  ) {
    corruptSmState(`${detail}.unhandledOutboundEntries must be a bounded array`);
  }
  const unhandledOutboundEntries = record.unhandledOutboundEntries.map((rawEntry, index) => {
    const entryDetail = `${detail}.unhandledOutboundEntries[${index}]`;
    const entry = exactObject(rawEntry, ["stanza", "sentAtEpochMs"], entryDetail);
    const sentAtEpochMs = entry.sentAtEpochMs;
    if (
      typeof sentAtEpochMs !== "number"
      || !Number.isSafeInteger(sentAtEpochMs)
      || sentAtEpochMs < 0
      || sentAtEpochMs > JS_DATE_LIMIT_MS
    ) {
      corruptSmState(`${entryDetail}.sentAtEpochMs is invalid`);
    }
    return {
      stanza: decodeStanza(entry.stanza, budget, `${entryDetail}.stanza`),
      sentAtEpochMs,
    };
  });
  return {
    previd,
    inboundH: decodeU32(record.inboundH, `${detail}.inboundH`),
    outboundH: decodeU32(record.outboundH, `${detail}.outboundH`),
    ...(maxResumeSeconds === undefined ? {} : { maxResumeSeconds }),
    unhandledOutboundEntries,
    ...(resource === undefined ? {} : { resource }),
  };
}

export function cloneSmResumeState(state: PersistedSmResumeState): PersistedSmResumeState {
  return structuredClone(state);
}
