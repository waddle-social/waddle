/** Stable public facade for privacy-safe, observe-only browser telemetry. */
export { initTelemetry } from "./telemetry/init";
export { handleUnhandledRejectionEvent, handleWindowErrorEvent, installGlobalErrorTelemetry, reportVueError } from "./telemetry/global-errors";
export { reportAuthBootstrap, reportCallAudioProcessing, reportCallMediaPath, reportError, reportMessageAcked, reportMessageFailed, reportQueueDepthChange, reportSendEnqueued, reportSessionLifecycle, reportStatusChange, type AuthBootstrapOutcome, type ErrorKind } from "./telemetry/events";
export { reportCatchup, reportReconnectScheduled, reportResumeDrain } from "./telemetry/health";
export { __clearSensitiveUrlsForTesting, __scrubMissingFetchSpanUrlForTesting, __scrubSpanUrlForTesting, __scrubXhrSpanUrlForTesting, markSensitiveUrlForTelemetry } from "./telemetry/span-privacy";
export { __faroSessionTrackingConfigForTesting, __sanitizeFaroTransportItemForTesting } from "./telemetry/transport";
export { __setFaroForTesting, __setGateZeroFaroScopeForTesting } from "./telemetry/testing";
export { withSpan } from "./telemetry/tracing";
