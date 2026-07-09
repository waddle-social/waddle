import {
  isForbiddenTelemetryAttributeKey,
  isPrivacySafeTraceResourceAttributeKey,
} from "./attribute-privacy";
import { sanitizeTelemetryText } from "./text-privacy";
import { strictTracePayload } from "./trace-wire-schema";

const SPAN_URL_ATTRIBUTE_KEYS = ["http.url", "url.full"] as const;
const SPAN_ENDPOINT_ATTRIBUTE_KEYS = [
  "http.host",
  "http.target",
  "server.address",
  "server.port",
  "url.path",
] as const;
const SPAN_QUERY_ATTRIBUTE_KEYS = ["url.fragment", "url.query"] as const;
const SPAN_REDACTED_ATTRIBUTE_KEYS = ["http.response.status_text", "http.status_text"] as const;
const SAFE_SPAN_ATTRIBUTE_KEYS = new Set([
  "http.host",
  "http.method",
  "http.request.method",
  "http.response.status_code",
  "http.status_code",
  "http.target",
  "http.url",
  "network.protocol.name",
  "network.protocol.version",
  "server.address",
  "server.port",
  "url.fragment",
  "url.full",
  "url.path",
  "url.query",
  "waddle.xmpp.transport",
]);
const SAFE_SPAN_EVENT_ATTRIBUTE_KEYS = new Set(["event.category"]);
const SAFE_SPAN_LINK_ATTRIBUTE_KEYS = new Set(["link.category"]);

interface SpanEndpoint {
  address: string;
  host: string;
  path: string;
  port: number;
}

export interface TracePrivacyPolicy {
  trustedOrigins: Set<string>;
  scrubUrl: (value: string | undefined, trustedOrigins: Set<string>) => string | undefined;
  safeSpanEndpoint: (value: string) => SpanEndpoint;
  redactedSpanEndpoint: () => SpanEndpoint;
}

type SpanAttributeValue = string | number;
type OtelAnyValue = {
  stringValue?: string | null;
  boolValue?: boolean | null;
  intValue?: number | null;
  doubleValue?: number | null;
};
type OtelKeyValue = {
  key: string;
  value: OtelAnyValue;
};
export function sanitizeTracePayload(
  payload: unknown,
  policy: TracePrivacyPolicy,
): Record<string, unknown> | null {
  if (!isRecord(payload) || !Array.isArray(payload.resourceSpans)) return null;
  for (const resourceSpan of payload.resourceSpans) {
    if (!isRecord(resourceSpan)) continue;
    const resource = isRecord(resourceSpan.resource) ? resourceSpan.resource : undefined;
    sanitizeOtelResourceAttributes(otelAttributes(resource?.attributes));
    if (!Array.isArray(resourceSpan.scopeSpans)) continue;
    for (const scopeSpan of resourceSpan.scopeSpans) {
      if (!isRecord(scopeSpan)) continue;
      const scope = isRecord(scopeSpan.scope) ? scopeSpan.scope : undefined;
      const scopeAttributes = otelAttributes(scope?.attributes);
      if (scopeAttributes) scopeAttributes.splice(0);
      if (!Array.isArray(scopeSpan.spans)) continue;
      for (const span of scopeSpan.spans) {
        if (!isRecord(span)) continue;
        sanitizeOtelSpanAttributes(otelAttributes(span.attributes), policy);
        if (isRecord(span.status) && span.status.message) span.status.message = "operation-error";
        sanitizeNestedTraceAttributes(span.events, SAFE_SPAN_EVENT_ATTRIBUTE_KEYS);
        sanitizeNestedTraceAttributes(span.links, SAFE_SPAN_LINK_ATTRIBUTE_KEYS);
      }
    }
  }
  return strictTracePayload(payload);
}

function sanitizeNestedTraceAttributes(
  entries: unknown,
  allowedKeys: ReadonlySet<string>,
): void {
  if (!Array.isArray(entries)) return;
  for (const entry of entries) {
    if (isRecord(entry)) sanitizeOtelAttributeStrings(otelAttributes(entry.attributes), allowedKeys);
  }
}

function otelAttributes(value: unknown): OtelKeyValue[] | undefined {
  return Array.isArray(value) ? value as OtelKeyValue[] : undefined;
}

function sanitizeOtelSpanAttributes(
  attributes: OtelKeyValue[] | undefined,
  policy: TracePrivacyPolicy,
): void {
  if (!attributes) return;
  retainAllowedOtelAttributes(attributes, SAFE_SPAN_ATTRIBUTE_KEYS);
  const url = firstOtelSpanUrl(attributes);

  if (url) {
    const scrubbed = policy.scrubUrl(url, policy.trustedOrigins);
    if (scrubbed) setOtelSpanUrlAttributes(attributes, scrubbed, policy);
  } else {
    redactOtelEndpointAttributes(attributes, policy);
  }

  for (const key of SPAN_QUERY_ATTRIBUTE_KEYS) {
    const attribute = findOtelAttribute(attributes, key);
    if (attribute) setOtelAttributeValue(attribute, ":redacted");
  }
  for (const key of SPAN_REDACTED_ATTRIBUTE_KEYS) {
    const attribute = findOtelAttribute(attributes, key);
    if (attribute) setOtelAttributeValue(attribute, ":redacted");
  }

  sanitizeOtelAttributeStrings(attributes, SAFE_SPAN_ATTRIBUTE_KEYS);
}

function sanitizeOtelAttributeStrings(
  attributes: OtelKeyValue[] | undefined,
  allowedKeys: ReadonlySet<string>,
): void {
  if (!attributes) return;
  retainAllowedOtelAttributes(attributes, allowedKeys);
  for (const attribute of attributes) {
    if (typeof attribute.value?.stringValue === "string") {
      setOtelAttributeValue(attribute, sanitizeTelemetryText(attribute.value.stringValue));
    }
  }
}

function sanitizeOtelResourceAttributes(attributes: OtelKeyValue[] | undefined): void {
  if (!attributes) return;
  retainUniqueOtelAttributes(attributes, isPrivacySafeTraceResourceAttributeKey);
  for (const attribute of attributes) {
    if (typeof attribute.value?.stringValue === "string") {
      setOtelAttributeValue(attribute, sanitizeTelemetryText(attribute.value.stringValue));
    }
  }
}

function retainAllowedOtelAttributes(
  attributes: OtelKeyValue[],
  allowedKeys: ReadonlySet<string>,
): void {
  retainUniqueOtelAttributes(
    attributes,
    (key) => allowedKeys.has(key) && !isForbiddenTelemetryAttributeKey(key),
  );
}

function retainUniqueOtelAttributes(
  attributes: OtelKeyValue[],
  keyAllowed: (key: string) => boolean,
): void {
  const seen = new Set<string>();
  for (let index = 0; index < attributes.length;) {
    const attribute = attributes[index];
    if (
      !isOtelKeyValue(attribute)
      || !keyAllowed(attribute.key)
      || seen.has(attribute.key)
    ) {
      attributes.splice(index, 1);
      continue;
    }
    seen.add(attribute.key);
    index += 1;
  }
}

function firstOtelSpanUrl(attributes: OtelKeyValue[]): string | undefined {
  for (const key of SPAN_URL_ATTRIBUTE_KEYS) {
    const value = readOtelStringAttribute(attributes, key);
    if (value) return value;
  }
  return undefined;
}

function setOtelSpanUrlAttributes(
  attributes: OtelKeyValue[],
  value: string,
  policy: TracePrivacyPolicy,
): void {
  const endpoint = policy.safeSpanEndpoint(value);
  upsertOtelAttribute(attributes, "http.url", value);
  upsertOtelAttribute(attributes, "url.full", value);
  upsertOtelAttribute(attributes, "http.host", endpoint.host);
  upsertOtelAttribute(attributes, "http.target", endpoint.path);
  upsertOtelAttribute(attributes, "server.address", endpoint.address);
  upsertOtelAttribute(attributes, "server.port", endpoint.port);
  upsertOtelAttribute(attributes, "url.path", endpoint.path);
}

function redactOtelEndpointAttributes(
  attributes: OtelKeyValue[],
  policy: TracePrivacyPolicy,
): void {
  const endpoint = policy.redactedSpanEndpoint();
  const replacements: Record<typeof SPAN_ENDPOINT_ATTRIBUTE_KEYS[number], SpanAttributeValue> = {
    "http.host": endpoint.host,
    "http.target": endpoint.path,
    "server.address": endpoint.address,
    "server.port": endpoint.port,
    "url.path": endpoint.path,
  };

  for (const key of SPAN_ENDPOINT_ATTRIBUTE_KEYS) {
    const attribute = findOtelAttribute(attributes, key);
    if (attribute) setOtelAttributeValue(attribute, replacements[key]);
  }
}

function readOtelStringAttribute(attributes: OtelKeyValue[], key: string): string | undefined {
  const value = findOtelAttribute(attributes, key)?.value?.stringValue;
  return typeof value === "string" ? value : undefined;
}

function findOtelAttribute(attributes: OtelKeyValue[], key: string): OtelKeyValue | undefined {
  return attributes.find((attribute) => attribute.key === key);
}

function upsertOtelAttribute(attributes: OtelKeyValue[], key: string, value: SpanAttributeValue): void {
  const existing = findOtelAttribute(attributes, key);
  if (existing) {
    setOtelAttributeValue(existing, value);
    return;
  }
  attributes.push({
    key,
    value: otelAttributeValue(value),
  });
}

function setOtelAttributeValue(attribute: OtelKeyValue, value: SpanAttributeValue): void {
  attribute.value = otelAttributeValue(value);
}

function otelAttributeValue(value: SpanAttributeValue): OtelAnyValue {
  return typeof value === "number" ? { intValue: value } : { stringValue: value };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isOtelKeyValue(value: unknown): value is OtelKeyValue {
  return isRecord(value) && typeof value.key === "string" && isRecord(value.value);
}
