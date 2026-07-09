import type { MetaAttributes, MetaPage } from "@grafana/faro-web-sdk";

function normalizeAttributeKey(key: string): string {
  return key.replace(/[^a-z0-9]/gi, "").toLowerCase();
}

export function sanitizedPageMeta(page?: MetaPage): MetaPage {
  const route = sanitizePagePathForTelemetry();
  return {
    attributes: sanitizeMetaAttributes(page?.attributes),
    id: route,
    url: `${currentPageOrigin()}${route}`,
  };
}

function sanitizeMetaAttributes(attributes: MetaAttributes | undefined): MetaAttributes | undefined {
  if (!attributes) return undefined;
  const safeAttributes: MetaAttributes = {};
  for (const [key, value] of Object.entries(attributes)) {
    if (normalizeAttributeKey(key) === "referrer") {
      safeAttributes.referrer = sanitizeReferrerForTelemetry(value);
    }
  }
  return Object.keys(safeAttributes).length > 0 ? safeAttributes : undefined;
}

function sanitizeReferrerForTelemetry(value: string): string {
  if (!value) return "";
  try {
    const referrer = new URL(value);
    const currentOrigin = currentPageOrigin();
    if (currentOrigin && referrer.origin === currentOrigin) {
      return `${referrer.origin}${sanitizeRoutePath(referrer.pathname)}`;
    }
    return "external";
  } catch {
    return ":redacted";
  }
}

export function sanitizePagePathForTelemetry(locationOverride?: Location): string {
  return sanitizeRoutePath(currentPagePath(locationOverride));
}

export function sanitizeRoutePath(path: string): string {
  const segments = path.split("/").filter(Boolean);
  if (segments.length === 0) return "/";

  switch (segments[0]) {
    case "admin":
      return segments.length > 1 ? "/admin/:panel" : "/admin";
    case "dm":
      return segments.length > 1 ? "/dm/:user" : "/dm";
    case "events":
    case "feed":
    case "settings":
    case "stories":
    case "threads":
      return `/${segments[0]}`;
    case "r":
      if (segments[2] === "x") return "/r/:room/x/:plugin/:route";
      return segments.length > 1 ? "/r/:room" : "/r";
    default:
      return "/:route";
  }
}

function currentPagePath(locationOverride?: Location): string {
  const pageLocation = locationOverride ?? (typeof window !== "undefined" ? window.location : undefined);
  const pathname = pageLocation?.pathname || "/";
  return pathname.startsWith("/") ? pathname : `/${pathname}`;
}

export function currentPageOrigin(): string {
  if (typeof window === "undefined") return "";
  if (window.location?.origin) return window.location.origin;
  try {
    return new URL(window.location?.href ?? "").origin;
  } catch {
    return "";
  }
}
