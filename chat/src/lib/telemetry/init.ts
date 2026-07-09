import { initializeFaro } from "@grafana/faro-web-sdk";
import { getDefaultOTELInstrumentations, TracingInstrumentation } from "@grafana/faro-web-tracing";
import { faroBuildIdentityMarker, parseFaroBuildIdentityScope } from "../../build-identity-contract";
import { fullCommitShaForTelemetry } from "./text-privacy";
import { installGlobalErrorTelemetry } from "./global-errors";
import { installClientHealthTelemetry } from "./health";
import { getFaro, observeTelemetry, setConfiguredTrustedSpanOrigins, setFaro, setGateZeroFaroScope, type FaroDeploymentScope } from "./runtime";
import { SENSITIVE_TRACE_URL_PATTERNS, normalizeUrlPrefixEntry, scrubFetchSpanUrl, scrubXhrSpanUrl, trustedSpanUrlOrigins } from "./span-privacy";
import { disabledFaroSessionTracking, getPrivacySafeWebInstrumentations, sanitizeFaroTransportItem } from "./transport";
import { sanitizePagePathForTelemetry, sanitizedPageMeta } from "./page-privacy";

interface InitTelemetryOptions {
  /** Faro collector URL (from Grafana Cloud Faro app config). */
  url: string;
  /** App name, typically the env-specific identifier e.g. `waddle-chat`. */
  appName: string;
  /** Semantic application version shown in Faro application metadata. */
  appVersion?: string;
  /** Deployment environment shown in Faro application metadata. */
  environment?: string;
  /** Commit SHA used as the Faro release, for source/deploy correlation. */
  release?: string;
  /** Static, bounded provenance filters attached only to Gate 0 signals. */
  deploymentScope?: FaroDeploymentScope;
  /** Build verifier marker retained in the client bundle, never exported. */
  buildIdentityMarker?: string;
  /**
   * Cross-origin URLs where the browser should inject W3C trace
   * context headers. Usually just the `waddle-server` origin. Passing
   * this — or leaving it empty — is what decides whether the frontend
   * actually shows up as the parent span in backend traces.
   */
  propagateTraceHeadersTo?: string[];
}
/**
 * Initialize Faro exactly once per page lifetime. Re-invocation is a
 * no-op — the module guards on `faro` being non-null.
 *
 * Missing `url` silently skips init. That's the shape callers rely on:
 * `initTelemetry({ url: import.meta.env.PUBLIC_FARO_URL, ... })` can be
 * fired unconditionally and does nothing when env vars are unset.
 */
export function initTelemetry(options: InitTelemetryOptions): void {
  if (getFaro()) return;
  if (!options.url) return;

  try {
    const deploymentScope = requireFaroDeploymentScope(options);
    setGateZeroFaroScope(deploymentScope);
    const propagateUrls = (options.propagateTraceHeadersTo ?? [])
      .filter((entry) => entry && entry.trim().length > 0)
      .map((entry) => normalizeUrlPrefixEntry(entry));
    const ignoredTraceUrls = [
      ...SENSITIVE_TRACE_URL_PATTERNS,
      normalizeUrlPrefixEntry(options.url),
    ];
    const trustedSpanOrigins = trustedSpanUrlOrigins(options.propagateTraceHeadersTo ?? []);
    setConfiguredTrustedSpanOrigins(trustedSpanOrigins);

    setFaro(initializeFaro({
      url: options.url,
      // Faro otherwise creates a per-page session ID and its FetchTransport
      // sends that value as x-faro-session-id outside beforeSend's reach.
      sessionTracking: disabledFaroSessionTracking(),
      app: {
        name: options.appName || "waddle-chat",
        version: options.appVersion || "unknown",
        environment: options.environment || undefined,
        release: deploymentScope.release,
      },
      pageTracking: {
        page: sanitizedPageMeta(),
        generatePageId: sanitizePagePathForTelemetry,
      },
      beforeSend: sanitizeFaroTransportItem,
      instrumentations: [
        // Default browser instrumentations, minus global error capture:
        // explicit app errors go through `reportError()` so messages and
        // context are sanitized before leaving the page. Console and
        // resource performance capture stay disabled for the same reason.
        ...getPrivacySafeWebInstrumentations(),
        // Wraps fetch + XMLHttpRequest in OTel spans and injects
        // traceparent/tracestate on requests whose URL matches one of
        // `propagateTraceHeaderCorsUrls`. Without a matching entry the
        // browser does NOT send those headers cross-origin, so the
        // backend can't join the trace.
        new TracingInstrumentation({
          instrumentations: getDefaultOTELInstrumentations({
            ignoreUrls: ignoredTraceUrls,
            propagateTraceHeaderCorsUrls: propagateUrls,
            fetchInstrumentationOptions: {
              applyCustomAttributesOnSpan: (span, request, result) =>
                scrubFetchSpanUrl(span, request, result, trustedSpanOrigins),
            },
            xhrInstrumentationOptions: {
              applyCustomAttributesOnSpan: (span, xhr) =>
                scrubXhrSpanUrl(span, xhr, trustedSpanOrigins),
            },
          }),
        }),
      ],
    }));
  } catch (err) {
    // Faro itself throwing here is already a telemetry bug; log to the
    // console so it surfaces in devtools but never propagate — chat
    // must continue to work with or without telemetry.
    console.error("Faro initialization failed", err);
    setFaro(null);
    setGateZeroFaroScope(null);
  }
  observeTelemetry(installClientHealthTelemetry);
  observeTelemetry(installGlobalErrorTelemetry);
}

function requireFaroDeploymentScope(options: InitTelemetryOptions): FaroDeploymentScope {
  const release = fullCommitShaForTelemetry(options.release);
  if (!release) throw new Error("Faro release must be the full build commit");
  const scope = parseFaroBuildIdentityScope(
    options.deploymentScope,
    release,
    "Faro deployment scope",
  );
  if (options.environment !== scope.deploymentEnvironment) {
    throw new Error("Faro app environment must match deploymentEnvironment");
  }
  const expectedMarker = faroBuildIdentityMarker(release, scope);
  if (options.buildIdentityMarker !== expectedMarker) {
    throw new Error("Faro build identity marker does not match deployment scope");
  }
  return { ...scope };
}
