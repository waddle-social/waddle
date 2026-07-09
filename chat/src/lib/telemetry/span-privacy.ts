import type { Span } from "@opentelemetry/api";
import { observeTelemetry } from "./runtime";
import { currentPageOrigin, sanitizeRoutePath } from "./page-privacy";

const UNKNOWN_EXTERNAL_URL = "external:unknown";
const UNKNOWN_FETCH_URL = "fetch:unknown";
const UNKNOWN_FILE_TRANSFER_URL = "file-transfer:unknown";
const UNKNOWN_XHR_URL = "xhr:unknown";
const REDACTED_SPAN_HOST = ":redacted";
const REDACTED_SPAN_PATH = ":unknown";
const REDACTED_SPAN_PORT = 0;
const sensitiveSpanUrls = new Set<string>();

export const SENSITIVE_TRACE_URL_PATTERNS = [
  /[?&]session_id=/,
  /[?&]api_key=/,
  /\/api\/upload(?:[/?#]|$)/,
  /\/api\/files(?:[/?#]|$)/,
  /^https:\/\/api\.giphy\.com\//,
];

/**
 * Turn a string into the URL-prefix RegExp `TracingInstrumentation`
 * expects. A plain string match is exact, so `https://xmpp.waddle.social`
 * would not match `/api/...`; the escaped prefix regex does.
 */
export function normalizeUrlPrefixEntry(entry: string): RegExp {
  const trimmed = entry.trim();
  const prefix = trimmed.endsWith("/") ? trimmed.slice(0, -1) : trimmed;
  return new RegExp(`^${escapeRegExp(prefix)}(?:[/?#]|$)`);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function trustedSpanUrlOrigins(entries: string[]): Set<string> {
  const origins = new Set<string>();
  const pageOrigin = currentPageOrigin();
  if (pageOrigin) origins.add(pageOrigin);

  for (const entry of entries) {
    try {
      origins.add(new URL(entry, currentPageHref()).origin);
    } catch {
      // Invalid config entry: leave it out rather than widening trust.
    }
  }
  return origins;
}

export function scrubFetchSpanUrl(
  span: Span,
  request: unknown,
  result: unknown,
  trustedOrigins: Set<string>,
): void {
  const url = readResultUrl(result) ?? readRequestUrl(request);
  if (!url) {
    setSpanUrlAttributes(span, UNKNOWN_FETCH_URL);
    return;
  }
  scrubSpanUrlAttributes(span, url, trustedOrigins);
}

export function scrubXhrSpanUrl(span: Span, xhr: XMLHttpRequest, trustedOrigins: Set<string>): void {
  if (!xhr.responseURL) {
    setSpanUrlAttributes(span, UNKNOWN_XHR_URL);
    return;
  }
  scrubSpanUrlAttributes(span, xhr.responseURL, trustedOrigins);
}

function readResultUrl(result: unknown): string | undefined {
  if (result && typeof result === "object" && "url" in result) {
    const url = (result as { url?: unknown }).url;
    return typeof url === "string" ? url : undefined;
  }
  return undefined;
}

function readRequestUrl(request: unknown): string | undefined {
  if (typeof Request !== "undefined" && request instanceof Request) {
    return request.url;
  }
  if (typeof request === "string") return request;
  if (typeof URL !== "undefined" && request instanceof URL) return request.toString();
  return undefined;
}

function scrubSpanUrlAttributes(span: Span, url: string | undefined, trustedOrigins: Set<string>): void {
  const scrubbed = scrubUrl(url, trustedOrigins);
  if (!scrubbed) return;
  setSpanUrlAttributes(span, scrubbed);
}

function setSpanUrlAttributes(span: Span, value: string): void {
  const endpoint = safeSpanEndpoint(value);
  observeTelemetry(() => {
    span.setAttribute("http.url", value);
    span.setAttribute("url.full", value);
    span.setAttribute("http.host", endpoint.host);
    span.setAttribute("http.target", endpoint.path);
    span.setAttribute("server.address", endpoint.address);
    span.setAttribute("server.port", endpoint.port);
    span.setAttribute("url.path", endpoint.path);
  });
}

export function safeSpanEndpoint(value: string): {
  address: string;
  host: string;
  path: string;
  port: number;
} {
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") return redactedSpanEndpoint();
    return {
      address: url.hostname,
      host: url.host,
      path: url.pathname || "/",
      port: url.port ? Number(url.port) : (url.protocol === "https:" ? 443 : 80),
    };
  } catch {
    return redactedSpanEndpoint();
  }
}

export function redactedSpanEndpoint(): {
  address: string;
  host: string;
  path: string;
  port: number;
} {
  return {
    address: REDACTED_SPAN_HOST,
    host: REDACTED_SPAN_HOST,
    path: REDACTED_SPAN_PATH,
    port: REDACTED_SPAN_PORT,
  };
}

export function scrubUrl(value: string | undefined, trustedOrigins: Set<string>): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value, currentPageHref());
    if (isSensitiveSpanUrl(url)) return UNKNOWN_FILE_TRANSFER_URL;
    if (!trustedOrigins.has(url.origin)) return scrubExternalUrl();
    url.pathname = scrubUrlPath(url.pathname);
    url.search = "";
    url.hash = "";
    return url.toString();
  } catch {
    return scrubUrlPath(value.split(/[?#]/, 1)[0] ?? "");
  }
}

function currentPageHref(): string {
  if (typeof window === "undefined") return "http://localhost";
  return window.location?.href || "http://localhost";
}

function scrubExternalUrl(): string {
  return UNKNOWN_EXTERNAL_URL;
}

function scrubUrlPath(path: string): string {
  if (path.startsWith("/api/upload/")) return "/api/upload/:slot";
  if (path === "/api/upload") return path;
  if (path.startsWith("/api/files/")) return "/api/files/:slot/:file";
  if (path === "/api/files") return path;
  if (path.startsWith("/api/")) return "/api/:endpoint";
  // Asset names are build-controlled in normal traffic, but the request URL
  // is still an untrusted input at this boundary. Never let an arbitrary
  // same-origin /_astro/ suffix become free text in a trace span.
  if (path.startsWith("/_astro/")) return "/_astro/:asset";
  if (isStaticAssetPath(path)) return path;
  return sanitizeRoutePath(path);
}

function isStaticAssetPath(path: string): boolean {
  return path === "/favicon.ico"
    || path === "/manifest.webmanifest"
    || path === "/waddle-logo.svg"
    || /^\/(?:apple-touch-icon|favicon-\d+x\d+)\.png$/.test(path);
}

function isSensitiveSpanUrl(url: URL): boolean {
  return sensitiveSpanUrls.has(normalizeSensitiveSpanUrl(url));
}

function normalizeSensitiveSpanUrl(url: URL): string {
  const copy = new URL(url);
  copy.search = "";
  copy.hash = "";
  return copy.toString();
}

export function markSensitiveUrlForTelemetry(value: string): void {
  try {
    sensitiveSpanUrls.add(normalizeSensitiveSpanUrl(new URL(value, currentPageHref())));
  } catch {
    // Invalid URLs never make it to fetch/XHR either; ignore.
  }
}

/** For tests only — clear dynamically marked sensitive URLs. */
export function __clearSensitiveUrlsForTesting(): void {
  sensitiveSpanUrls.clear();
}

/** For tests only — exercise span URL redaction without real OTel spans. */
export function __scrubSpanUrlForTesting(value: string, trustedOrigins: string[]): string | undefined {
  return scrubUrl(value, new Set(trustedOrigins));
}

/** For tests only — exercise XHR span URL redaction without real XHR instrumentation. */
export function __scrubXhrSpanUrlForTesting(responseURL: string, trustedOrigins: string[]): Record<string, string | number> {
  const attributes: Record<string, string | number> = {};
  const span = {
    setAttribute: (key: string, value: string | number) => {
      attributes[key] = value;
      return span;
    },
  } as unknown as Span;
  scrubXhrSpanUrl(span, { responseURL } as XMLHttpRequest, new Set(trustedOrigins));
  return attributes;
}

/** For tests only — exercise fetch span URL fallback without real fetch instrumentation. */
export function __scrubMissingFetchSpanUrlForTesting(): Record<string, string | number> {
  const attributes: Record<string, string | number> = {};
  const span = {
    setAttribute: (key: string, value: string | number) => {
      attributes[key] = value;
      return span;
    },
  } as unknown as Span;
  scrubFetchSpanUrl(span, undefined, undefined, new Set());
  return attributes;
}
