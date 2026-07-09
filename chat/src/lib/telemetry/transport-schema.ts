import { MAX_FARO_MEASUREMENT_VALUE } from "./measurement-contract";

type StringRule = (value: unknown) => string | undefined;

type PayloadSchema = {
  fields: Readonly<Record<string, StringRule>>;
  required: readonly string[];
};

type MeasurementSchema = {
  context: PayloadSchema;
  requiredValues: readonly string[];
  valueNames: ReadonlySet<string>;
};

const FULL_COMMIT_SHA = /^[0-9a-f]{40}$/;
const BOUNDED_LABEL = /^[a-z0-9](?:[a-z0-9_-]{0,62}[a-z0-9])?$/;
const RFC3339_TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/;
const TRACE_ID = /^[0-9a-f]{32}$/;
const SPAN_ID = /^[0-9a-f]{16}$/;

const enumRule = (...values: string[]): StringRule => {
  const allowed = new Set(values);
  return (value) => typeof value === "string" && allowed.has(value) ? value : undefined;
};

const boundedLabel: StringRule = (value) =>
  typeof value === "string" && BOUNDED_LABEL.test(value) ? value : undefined;
const commitSha: StringRule = (value) =>
  typeof value === "string" && FULL_COMMIT_SHA.test(value) ? value : undefined;

const deploymentScopeFields = {
  deploymentEnvironment: boundedLabel,
  cluster: boundedLabel,
  namespace: boundedLabel,
  sourceId: boundedLabel,
  release: commitSha,
} as const;
const deploymentScopeNames = Object.keys(deploymentScopeFields);

const eventSchemas: Readonly<Record<string, PayloadSchema>> = {
  "chat.journey.auth": schema(
    { outcome: enumRule("ready", "signed_out", "expired", "failed"), ...deploymentScopeFields },
    ["outcome", ...deploymentScopeNames],
  ),
  "chat.xmpp.message.acked": schema(
    { kind: enumRule("room", "dm"), ...deploymentScopeFields },
    ["kind", ...deploymentScopeNames],
  ),
  "chat.xmpp.message.failed": schema({ kind: enumRule("room", "dm") }, ["kind"]),
  "chat.xmpp.send.enqueued": schema({
    kind: enumRule("room", "dm"),
    reason: enumRule("offline", "destroying", "no-client", "reconnecting", "not-ready"),
  }, ["kind", "reason"]),
  "chat.xmpp.session.lifecycle": schema(
    { type: enumRule("fresh", "resumed"), ...deploymentScopeFields },
    ["type", ...deploymentScopeNames],
  ),
  "chat.call.audio_processing": schema({
    kind: enumRule("active", "no-mic"),
    noise_suppression: enumRule("on", "off", "unknown"),
    echo_cancellation: enumRule("on", "off", "unknown"),
    auto_gain_control: enumRule("on", "off", "unknown"),
    ai_noise_filter: enumRule("off", "rnnoise", "dtln", "deepfilternet"),
  }, ["kind", "ai_noise_filter"]),
  "chat.call.media_path": schema({
    direction: enumRule("send", "recv"),
    source: enumRule("camera", "screen", "microphone"),
    codec: enumRule(
      "unknown", "VP8", "VP9", "H264", "AV1", "opus", "red", "PCMU", "PCMA", "G722",
      "telephone-event", "CN",
    ),
    ice_candidate_type: enumRule("unknown", "host", "srflx", "prflx", "relay"),
    ice_transport: enumRule("unknown", "udp", "tcp"),
    audio_bitrate_band: enumRule("silent", "standard", "high"),
    video_resolution_band: enumRule("180p", "360p", "540p", "720p", "1080p", "1440p"),
  }, ["direction", "source", "codec", "ice_candidate_type", "ice_transport"]),
  "chat.xmpp.status": schema({ state: enumRule("online", "offline", "reconnecting", "error") }, ["state"]),
  "chat.client.visibility": visibilitySchema(),
  "chat.xmpp.reconnect.scheduled": visibilitySchema(),
};

const measurementSchemas: Readonly<Record<string, MeasurementSchema>> = {
  "chat.journey.auth.duration_ms": measurement(
    ["duration_ms"],
    schema(
      { outcome: enumRule("ready", "signed_out", "expired", "failed"), ...deploymentScopeFields },
      ["outcome", ...deploymentScopeNames],
    ),
  ),
  "chat.xmpp.message.acked.latency_ms": measurement(
    ["latency_ms"],
    schema({ kind: enumRule("room", "dm"), ...deploymentScopeFields }, ["kind", ...deploymentScopeNames]),
  ),
  "chat.xmpp.queue.depth": measurement(["persisted", "inflight"]),
  "chat.xmpp.reconnect.duration_ms": measurement(
    ["duration_ms"],
    schema(deploymentScopeFields, deploymentScopeNames),
  ),
  "chat.client.longtask.duration_ms": measurement(["duration_ms", "hidden_ms"], visibilitySchema()),
  "chat.client.heap": measurement(["used_mb", "total_mb", "limit_mb", "hidden_ms"], visibilitySchema()),
  "chat.xmpp.reconnect.attempt": measurement(
    ["count", "attempt", "delay_ms", "hidden_ms"],
    visibilitySchema(),
  ),
  "chat.xmpp.catchup": measurement(
    ["conversations", "processed_conversations", "pages", "messages", "duration_ms", "hidden_ms"],
    schema({
      visibility: enumRule("visible", "hidden"),
      hidden_bucket: enumRule("visible", "lt_1m", "1m_5m", "5m_15m", "15m_1h", "gt_1h"),
      outcome: enumRule("completed", "aborted", "failed"),
    }, ["visibility", "hidden_bucket", "outcome"]),
  ),
  "chat.xmpp.resume_drain": measurement(["buffered", "duration_ms", "hidden_ms"], visibilitySchema()),
};

export function sanitizeKnownEventPayload(payload: unknown): Record<string, unknown> | null {
  if (!isRecord(payload) || typeof payload.name !== "string") return null;
  const eventSchema = eventSchemas[payload.name];
  if (!eventSchema) return null;
  const attributes = sanitizeStringFields(payload.attributes, eventSchema);
  if (!attributes) return null;
  return compactEnvelope({
    name: payload.name,
    timestamp: sanitizeTimestamp(payload.timestamp),
    attributes,
    trace: sanitizeTraceContext(payload.trace),
  });
}

export function sanitizeKnownMeasurementPayload(payload: unknown): Record<string, unknown> | null {
  if (!isRecord(payload) || typeof payload.type !== "string") return null;
  const measurementSchema = measurementSchemas[payload.type];
  if (!measurementSchema || !isRecord(payload.values)) return null;
  const values: Record<string, number> = {};
  for (const name of measurementSchema.valueNames) {
    const value = boundedMeasurementValue(payload.values[name]);
    if (value !== undefined) values[name] = value;
  }
  if (measurementSchema.requiredValues.some((name) => values[name] === undefined)) return null;
  const context = sanitizeStringFields(payload.context, measurementSchema.context);
  if (!context) return null;
  return compactEnvelope({
    type: payload.type,
    values,
    timestamp: sanitizeTimestamp(payload.timestamp),
    context: Object.keys(context).length > 0 ? context : undefined,
    trace: sanitizeTraceContext(payload.trace),
  });
}

export function sanitizeTraceContext(value: unknown): Record<string, string> | undefined {
  if (!isRecord(value)) return undefined;
  const traceId = typeof value.trace_id === "string" && TRACE_ID.test(value.trace_id)
    ? value.trace_id
    : undefined;
  const spanId = typeof value.span_id === "string" && SPAN_ID.test(value.span_id)
    ? value.span_id
    : undefined;
  return traceId && spanId ? { trace_id: traceId, span_id: spanId } : undefined;
}

export function sanitizeTimestamp(value: unknown): string | undefined {
  return typeof value === "string" && RFC3339_TIMESTAMP.test(value) ? value : undefined;
}

function schema(
  fields: Readonly<Record<string, StringRule>>,
  required: readonly string[],
): PayloadSchema {
  return { fields, required };
}

function visibilitySchema(): PayloadSchema {
  return schema({
    visibility: enumRule("visible", "hidden"),
    hidden_bucket: enumRule("visible", "lt_1m", "1m_5m", "5m_15m", "15m_1h", "gt_1h"),
  }, ["visibility", "hidden_bucket"]);
}

function measurement(
  values: readonly string[],
  context: PayloadSchema = schema({}, []),
): MeasurementSchema {
  return { valueNames: new Set(values), requiredValues: values, context };
}

function sanitizeStringFields(value: unknown, payloadSchema: PayloadSchema): Record<string, string> | null {
  if (!isRecord(value)) return payloadSchema.required.length === 0 ? {} : null;
  const safe: Record<string, string> = {};
  for (const [name, rule] of Object.entries(payloadSchema.fields)) {
    const normalized = rule(value[name]);
    if (normalized !== undefined) safe[name] = normalized;
  }
  return payloadSchema.required.some((name) => safe[name] === undefined) ? null : safe;
}

function boundedMeasurementValue(value: unknown): number | undefined {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return undefined;
  return Math.min(Math.round(value), MAX_FARO_MEASUREMENT_VALUE);
}

function compactEnvelope(value: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
