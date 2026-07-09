import { callAudioProcessingEventAttributes, type VerifiedCallAudioProcessing } from "../calls/call-audio-processing-telemetry";
import { callMediaPathEventAttributes, type CallMediaPathSnapshot } from "../calls/call-media-path-telemetry";
import type { SendQueueReason } from "../xmpp/send-types";
import { categorizedErrorForTelemetry, privacySafeErrorContext, type ErrorKind } from "./error-classification";
import { getFaro, getGateZeroFaroScope, observeTelemetry, pushEventObserveOnly, pushMeasurementObserveOnly } from "./runtime";

type MessageKind = "room" | "dm";
export type AuthBootstrapOutcome = "ready" | "signed_out" | "expired" | "failed";
export type { ErrorKind } from "./error-classification";

/**
 * Push an explicit error to Faro. Prefer this over `console.error`
 * for non-recoverable or user-visible failures — it produces both an
 * error beacon (browsable in Frontend Observability) and an exception
 * event on the currently active span, so the backend trace and the
 * frontend error are linked in Tempo.
 *
 * `recoverable=true` marks transient issues (e.g. reconnecting) that
 * shouldn't page anyone. `recoverable=false` is for terminal states
 * (session expired, stream closed fatally, storage quota exhausted).
 */
export function reportError(
  kind: ErrorKind,
  _error: unknown,
  context: { recoverable: boolean; detail?: string; [attr: string]: unknown } = {
    recoverable: true,
  },
): void {
  observeTelemetry(() => {
    const instance = getFaro();
    if (!instance) return;
    const contextStrings = privacySafeErrorContext(kind, context);
    const err = categorizedErrorForTelemetry(kind, contextStrings.detail);
    instance.api.pushError(err, { type: kind, context: contextStrings });
  });
}
function gateZeroAttributes(
  attributes: Record<string, string> = {},
): Record<string, string> | null {
  const scope = getGateZeroFaroScope();
  return scope ? { ...attributes, ...scope } : null;
}

function pushGateZeroEvent(name: string, attributes: Record<string, string> = {}): void {
  const scopedAttributes = gateZeroAttributes(attributes);
  if (!getFaro() || !scopedAttributes) return;
  pushEventObserveOnly(name, scopedAttributes);
}

function pushGateZeroMeasurement(
  measurement: { type: string; values: Record<string, number> },
  attributes: Record<string, string> = {},
): void {
  const context = gateZeroAttributes(attributes);
  if (!getFaro() || !context) return;
  pushMeasurementObserveOnly(measurement, { context });
}
export function reportMessageFailed(payload: {
  id: string;
  kind: MessageKind;
}): void {
  observeTelemetry(() => {
    pushEventObserveOnly("chat.xmpp.message.failed", { kind: payload.kind });
  });
}
export function reportAuthBootstrap(payload: {
  outcome: AuthBootstrapOutcome;
  durationMs: number;
}): void {
  observeTelemetry(() => {
    if (!getFaro()) return;
    pushGateZeroEvent("chat.journey.auth", { outcome: payload.outcome });
    pushGateZeroMeasurement({
      type: "chat.journey.auth.duration_ms",
      values: { duration_ms: Math.max(0, Math.round(payload.durationMs)) },
    }, { outcome: payload.outcome });
  });
}

export function reportMessageAcked(payload: {
  id: string;
  kind: MessageKind;
  latencyMs: number;
}): void {
  observeTelemetry(() => {
    pushGateZeroEvent("chat.xmpp.message.acked", { kind: payload.kind });
    pushGateZeroMeasurement({
      type: "chat.xmpp.message.acked.latency_ms",
      values: { latency_ms: payload.latencyMs },
    }, { kind: payload.kind });
  });
}

export function reportSendEnqueued(payload: {
  kind: MessageKind;
  reason: SendQueueReason;
}): void {
  observeTelemetry(() => {
    pushEventObserveOnly("chat.xmpp.send.enqueued", {
      kind: payload.kind,
      reason: payload.reason,
    });
  });
}

export function reportQueueDepthChange(payload: {
  persisted: number;
  inflight: number;
}): void {
  observeTelemetry(() => {
    pushMeasurementObserveOnly({
      type: "chat.xmpp.queue.depth",
      values: {
        persisted: payload.persisted,
        inflight: payload.inflight,
      },
    });
  });
}

export function reportSessionLifecycle(payload: {
  type: "fresh" | "resumed";
}): void {
  observeTelemetry(() => {
    pushGateZeroEvent("chat.xmpp.session.lifecycle", { type: payload.type });
  });
}

/**
 * Beacon the verified audio-processing state of the local call mic (issues
 * #913 / #914) so we can measure, across the fleet, what fraction of calls
 * actually have noise cancellation applied and which layer (browser constraint
 * vs AI model) is doing it. The payload is the PII-free state from
 * {@link callAudioProcessingEventAttributes}; coarse browser/platform context
 * rides along in Faro's event meta. Pure observability — no XMPP/Jingle wire
 * effect. De-dup (at most once per call per distinct state) is the caller's job
 * via `createCallAudioProcessingBeacon`.
 */
export function reportCallAudioProcessing(state: VerifiedCallAudioProcessing): void {
  observeTelemetry(() => {
    pushEventObserveOnly("chat.call.audio_processing", callAudioProcessingEventAttributes(state));
  });
}

/**
 * Beacon the media path a call track actually got (#996): the negotiated video
 * codec and the succeeded ICE candidate-pair (type + transport). This is the
 * baseline the #995 codec/Opus/ICE levers verify against and what surfaces the
 * silent "stuck on TCP relay" rate. The payload is the PII-free attribute set
 * from {@link callMediaPathEventAttributes}; coarse browser/platform context
 * rides along in Faro's event meta. Pure observability — no XMPP/Jingle wire
 * effect. De-dup (at most once per call per distinct path) is the caller's job
 * via `createCallMediaPathBeacon`.
 */
export function reportCallMediaPath(snapshot: CallMediaPathSnapshot): void {
  observeTelemetry(() => {
    pushEventObserveOnly("chat.call.media_path", callMediaPathEventAttributes(snapshot));
  });
}

export function reportStatusChange(payload: {
  state: string;
  reconnectDurationMs?: number;
}): void {
  observeTelemetry(() => {
    if (!getFaro()) return;
    pushEventObserveOnly("chat.xmpp.status", {
      state: payload.state,
    });
    if (typeof payload.reconnectDurationMs === "number") {
      pushGateZeroMeasurement({
        type: "chat.xmpp.reconnect.duration_ms",
        values: { duration_ms: payload.reconnectDurationMs },
      });
    }
  });
}
