import { getWebInstrumentations, type TransportItem } from "@grafana/faro-web-sdk";
import { fullCommitShaForTelemetry, sanitizeTelemetryText } from "./text-privacy";
import { sanitizeTransportPayload } from "./transport-privacy";
import { currentPageOrigin, sanitizedPageMeta } from "./page-privacy";
import { getConfiguredTrustedSpanOrigins } from "./runtime";
import { redactedSpanEndpoint, safeSpanEndpoint, scrubUrl } from "./span-privacy";

const FARO_CSP_INSTRUMENTATION = "@grafana/faro-web-sdk:instrumentation-csp";
const FARO_GLOBAL_ERROR_INSTRUMENTATION = "@grafana/faro-web-sdk:instrumentation-errors";
const FARO_NAVIGATION_INSTRUMENTATION = "@grafana/faro-web-sdk:instrumentation-navigation";
const FARO_SESSION_INSTRUMENTATION = "@grafana/faro-web-sdk:instrumentation-session";
const BOUNDED_META_TEXT = /^[A-Za-z0-9@._:/+-]{1,128}$/;

export function sanitizeFaroTransportItem(item: TransportItem): TransportItem | null {
  // Console capture is disabled, and Waddle has no structured log API. Drop
  // any SDK/future log item because its message is necessarily free text.
  if (item.type === "log") return null;

  const sanitized = {
    ...item,
    // Faro's default meta includes stable session and installation IDs. Keep
    // only deployment/static SDK context plus a route-templated page. This is
    // deliberately an allowlist: new SDK metadata is private until reviewed.
    meta: privacySafeFaroMeta(item.meta),
  };
  const payloadAccepted = sanitizeTransportPayload(sanitized, {
    trustedOrigins: trustedSpanOriginsForTransport(),
    scrubUrl,
    safeSpanEndpoint,
    redactedSpanEndpoint,
  });
  return payloadAccepted ? sanitized : null;
}

function trustedSpanOriginsForTransport(): Set<string> {
  const origins = getConfiguredTrustedSpanOrigins();
  const pageOrigin = currentPageOrigin();
  if (pageOrigin) origins.add(pageOrigin);
  return origins;
}

function privacySafeFaroMeta(meta: TransportItem["meta"]): TransportItem["meta"] {
  const safeMeta: TransportItem["meta"] = {
    page: sanitizedPageMeta(meta.page),
  };

  if (meta.app) {
    safeMeta.app = {
      environment: sanitizeOptionalMetaText(meta.app.environment),
      gitHash: fullCommitShaForTelemetry(meta.app.gitHash),
      name: sanitizeOptionalMetaText(meta.app.name),
      release: fullCommitShaForTelemetry(meta.app.release),
      version: sanitizeOptionalMetaText(meta.app.version),
    };
  }

  if (meta.sdk) {
    safeMeta.sdk = {
      name: sanitizeOptionalMetaText(meta.sdk.name),
      version: sanitizeOptionalMetaText(meta.sdk.version),
      integrations: meta.sdk.integrations?.map((integration) => ({
        name: sanitizeOptionalMetaText(integration.name),
        version: sanitizeOptionalMetaText(integration.version),
      })),
    };
  }

  return safeMeta;
}

function sanitizeOptionalMetaText(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const sanitized = sanitizeTelemetryText(value).slice(0, 128);
  return BOUNDED_META_TEXT.test(sanitized) ? sanitized : undefined;
}

export function getPrivacySafeWebInstrumentations() {
  return getWebInstrumentations({
    captureConsole: false,
    enableContentSecurityPolicyInstrumentation: false,
    enablePerformanceInstrumentation: false,
  }).filter((instrumentation) =>
    instrumentation.name !== FARO_CSP_INSTRUMENTATION
    && instrumentation.name !== FARO_GLOBAL_ERROR_INSTRUMENTATION
    && instrumentation.name !== FARO_NAVIGATION_INSTRUMENTATION
    && instrumentation.name !== FARO_SESSION_INSTRUMENTATION
  );
}

export function disabledFaroSessionTracking(): { enabled: false } {
  return { enabled: false };
}

/** For tests only — exercise the final transport guard without calling Grafana. */
export function __sanitizeFaroTransportItemForTesting(item: TransportItem): TransportItem | null {
  return sanitizeFaroTransportItem(item);
}

/** For tests only — prove the production config cannot create Faro sessions. */
export function __faroSessionTrackingConfigForTesting(): { enabled: false } {
  return disabledFaroSessionTracking();
}
