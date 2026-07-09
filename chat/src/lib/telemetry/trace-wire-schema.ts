import { sanitizeTelemetryText } from "./text-privacy";

const TRACE_ID = /^[0-9a-f]{32}$/;
const SPAN_ID = /^[0-9a-f]{16}$/;
const FULL_COMMIT_SHA = /^[0-9a-f]{40}$/;
const BOUNDED_LABEL = /^[a-z0-9](?:[a-z0-9_-]{0,62}[a-z0-9])?$/;
const SEMANTIC_VERSION = /^\d{1,5}\.\d{1,5}\.\d{1,5}(?:[-+][0-9A-Za-z.-]{1,32})?$/;
const SAFE_HOST = /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)*[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?::\d{1,5})?$/i;
const SAFE_HTTP_METHODS = new Set(["CONNECT", "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "TRACE"]);
const SAFE_PROTOCOL_NAMES = new Set(["http", "https", "ws", "wss"]);
const SAFE_PROTOCOL_VERSIONS = new Set(["1", "1.0", "1.1", "2", "2.0", "3", "3.0"]);
const SAFE_TELEMETRY_NAMES = new Set([
  "opentelemetry",
  "faro-web-sdk",
  "@opentelemetry/sdk-trace-web",
]);
const SAFE_SENTINEL_URLS = new Set([
  "external:unknown",
  "fetch:unknown",
  "file-transfer:unknown",
  "xhr:unknown",
]);

/** Rebuild a sanitized OTLP trace using only Faro's required wire fields. */
export function strictTracePayload(payload: unknown): Record<string, unknown> | null {
  if (!isRecord(payload) || !Array.isArray(payload.resourceSpans)) return null;
  const resourceSpans = payload.resourceSpans.flatMap((resourceSpan) => {
    if (!isRecord(resourceSpan) || !Array.isArray(resourceSpan.scopeSpans)) return [];
    const scopeSpans = resourceSpan.scopeSpans.flatMap((scopeSpan) => {
      if (!isRecord(scopeSpan) || !Array.isArray(scopeSpan.spans)) return [];
      const spans = scopeSpan.spans.flatMap((span) => {
        const strict = isRecord(span) ? strictSpan(span) : null;
        return strict ? [strict] : [];
      });
      if (spans.length === 0) return [];
      const scope = isRecord(scopeSpan.scope)
        ? {
            name: "waddle-browser",
            droppedAttributesCount: boundedInteger(scopeSpan.scope.droppedAttributesCount) ?? 0,
          }
        : undefined;
      return [{ ...(scope ? { scope } : {}), spans }];
    });
    if (scopeSpans.length === 0) return [];
    const resource = isRecord(resourceSpan.resource)
      ? {
          attributes: strictAttributes(resourceSpan.resource.attributes),
          droppedAttributesCount: boundedInteger(resourceSpan.resource.droppedAttributesCount) ?? 0,
        }
      : undefined;
    return [{ ...(resource ? { resource } : {}), scopeSpans }];
  });
  return resourceSpans.length > 0 ? { resourceSpans } : null;
}

function strictSpan(span: Record<string, unknown>): Record<string, unknown> | null {
  const traceId = safeTraceId(span.traceId);
  const spanId = safeSpanId(span.spanId);
  const startTimeUnixNano = safeFixed64(span.startTimeUnixNano);
  const endTimeUnixNano = safeFixed64(span.endTimeUnixNano);
  if (!traceId || !spanId || startTimeUnixNano === undefined || endTimeUnixNano === undefined) {
    return null;
  }
  const attributes = strictAttributes(span.attributes);
  const method = readStrictAttribute(attributes, "http.request.method")
    ?? readStrictAttribute(attributes, "http.method");
  const name = span.name === "xmpp.connect"
    ? "xmpp.connect"
    : typeof method === "string" && /^[A-Z]{3,8}$/.test(method)
      ? `http.${method.toLowerCase()}`
      : "browser.operation";
  const parentSpanId = safeSpanId(span.parentSpanId);
  const events = Array.isArray(span.events)
    ? span.events.flatMap((event) => {
        const strict = strictSpanEvent(event);
        return strict ? [strict] : [];
      })
    : [];
  const links = Array.isArray(span.links)
    ? span.links.flatMap((link) => {
        const strict = strictSpanLink(link);
        return strict ? [strict] : [];
      })
    : [];
  const flags = boundedInteger(span.flags, 255);
  return {
    traceId,
    spanId,
    ...(parentSpanId ? { parentSpanId } : {}),
    name,
    kind: boundedInteger(span.kind, 5) ?? 0,
    startTimeUnixNano,
    endTimeUnixNano,
    attributes,
    droppedAttributesCount: boundedInteger(span.droppedAttributesCount) ?? 0,
    events,
    droppedEventsCount: boundedInteger(span.droppedEventsCount) ?? 0,
    links,
    droppedLinksCount: boundedInteger(span.droppedLinksCount) ?? 0,
    status: strictSpanStatus(span.status),
    ...(flags === undefined ? {} : { flags }),
  };
}

function strictSpanEvent(value: unknown): Record<string, unknown> | null {
  if (!isRecord(value)) return null;
  const timeUnixNano = safeFixed64(value.timeUnixNano);
  if (timeUnixNano === undefined) return null;
  return {
    timeUnixNano,
    name: value.name === "exception" ? "exception" : "span.event",
    attributes: strictAttributes(value.attributes),
    droppedAttributesCount: boundedInteger(value.droppedAttributesCount) ?? 0,
  };
}

function strictSpanLink(value: unknown): Record<string, unknown> | null {
  if (!isRecord(value)) return null;
  const traceId = safeTraceId(value.traceId);
  const spanId = safeSpanId(value.spanId);
  if (!traceId || !spanId) return null;
  const flags = boundedInteger(value.flags, 255);
  return {
    traceId,
    spanId,
    attributes: strictAttributes(value.attributes),
    droppedAttributesCount: boundedInteger(value.droppedAttributesCount) ?? 0,
    ...(flags === undefined ? {} : { flags }),
  };
}

function strictSpanStatus(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) return { code: 0 };
  const code = boundedInteger(value.code, 2) ?? 0;
  return value.message ? { code, message: "operation-error" } : { code };
}

function strictAttributes(value: unknown): Array<Record<string, unknown>> {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  return value.flatMap((entry) => {
    if (!isRecord(entry) || typeof entry.key !== "string" || !isRecord(entry.value)) return [];
    if (seen.has(entry.key)) return [];
    seen.add(entry.key);
    const safeValue = strictAnyValue(entry.key, entry.value);
    return safeValue ? [{ key: entry.key, value: safeValue }] : [];
  });
}

function strictAnyValue(
  key: string,
  value: Record<string, unknown>,
): Record<string, unknown> | null {
  const stringValue = typeof value.stringValue === "string"
    ? sanitizeTelemetryText(value.stringValue).slice(0, 256)
    : undefined;
  const integerValue = typeof value.intValue === "number" && Number.isFinite(value.intValue)
    ? Math.round(value.intValue)
    : undefined;

  if (key === "http.method" || key === "http.request.method") {
    return stringValue && SAFE_HTTP_METHODS.has(stringValue) ? { stringValue } : null;
  }
  if (key === "http.status_code" || key === "http.response.status_code") {
    const status = integerValue ?? (stringValue && /^\d{3}$/.test(stringValue) ? Number(stringValue) : undefined);
    return status !== undefined && status >= 100 && status <= 599 ? { intValue: status } : null;
  }
  if (key === "network.protocol.name") {
    return stringValue && SAFE_PROTOCOL_NAMES.has(stringValue) ? { stringValue } : null;
  }
  if (key === "network.protocol.version") {
    return stringValue && SAFE_PROTOCOL_VERSIONS.has(stringValue) ? { stringValue } : null;
  }
  if (key === "waddle.xmpp.transport") return stringValue === "websocket" ? { stringValue } : null;
  if (key === "event.category") return stringValue === "network" ? { stringValue } : null;
  if (key === "link.category") return stringValue === "retry" ? { stringValue } : null;
  if (key === "http.url" || key === "url.full") {
    return stringValue && isSafeSanitizedUrl(stringValue) ? { stringValue } : null;
  }
  if (key === "http.host" || key === "server.address") {
    return stringValue && (stringValue === ":redacted" || SAFE_HOST.test(stringValue))
      ? { stringValue }
      : null;
  }
  if (key === "http.target" || key === "url.path") {
    return stringValue && isSafeSanitizedPath(stringValue) ? { stringValue } : null;
  }
  if (key === "url.query" || key === "url.fragment") {
    return stringValue === ":redacted" ? { stringValue } : null;
  }
  if (key === "server.port") {
    return integerValue !== undefined && integerValue >= 0 && integerValue <= 65_535
      ? { intValue: integerValue }
      : null;
  }
  if (
    key === "service.name"
    || key === "service.namespace"
    || key === "deployment.environment"
    || key === "deployment.environment.name"
  ) {
    return stringValue && BOUNDED_LABEL.test(stringValue) ? { stringValue } : null;
  }
  if (key === "service.version") {
    return stringValue && (FULL_COMMIT_SHA.test(stringValue) || SEMANTIC_VERSION.test(stringValue))
      ? { stringValue }
      : null;
  }
  if (key === "telemetry.sdk.name" || key === "telemetry.distro.name") {
    return stringValue && SAFE_TELEMETRY_NAMES.has(stringValue) ? { stringValue } : null;
  }
  if (key === "telemetry.sdk.language") {
    return stringValue === "webjs" || stringValue === "javascript" ? { stringValue } : null;
  }
  if (key === "telemetry.sdk.version" || key === "telemetry.distro.version") {
    return stringValue && SEMANTIC_VERSION.test(stringValue) ? { stringValue } : null;
  }
  return null;
}

function isSafeSanitizedUrl(value: string): boolean {
  if (SAFE_SENTINEL_URLS.has(value)) return true;
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:")
      && !url.username
      && !url.password
      && !url.search
      && !url.hash
      && SAFE_HOST.test(url.host)
      && isSafeSanitizedPath(url.pathname);
  } catch {
    return false;
  }
}

function isSafeSanitizedPath(value: string): boolean {
  return value === ":unknown"
    || value === "/"
    || value === "/:route"
    || /^(?:\/admin(?:\/:panel)?|\/dm(?:\/:user)?|\/r(?:\/:room(?:\/x\/:plugin\/:route)?)?|\/(?:events|feed|settings|stories|threads))$/.test(value)
    || /^\/api(?:\/:endpoint|\/upload\/:slot|\/files\/:slot\/:file)?$/.test(value)
    || value === "/_astro/:asset"
    || /^\/(?:favicon\.ico|manifest\.webmanifest|waddle-logo\.svg|apple-touch-icon\.png|favicon-\d+x\d+\.png)$/.test(value);
}

function readStrictAttribute(
  attributes: Array<Record<string, unknown>>,
  key: string,
): unknown {
  const attribute = attributes.find((entry) => entry.key === key);
  return isRecord(attribute?.value) ? attribute.value.stringValue : undefined;
}

function safeTraceId(value: unknown): string | undefined {
  return typeof value === "string" && TRACE_ID.test(value) ? value : undefined;
}

function safeSpanId(value: unknown): string | undefined {
  return typeof value === "string" && SPAN_ID.test(value) ? value : undefined;
}

function safeFixed64(value: unknown): string | number | undefined {
  if (typeof value === "string" && /^\d{1,30}$/.test(value)) return value;
  if (typeof value === "number" && Number.isFinite(value) && value >= 0) return Math.round(value);
  return undefined;
}

function boundedInteger(value: unknown, maximum = 1_000_000_000): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? Math.min(Math.round(value), maximum)
    : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
