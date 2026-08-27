import type { XmppStanzaErrorType } from "./xmpp/types";

type ErrorSource = "server" | "local-timeout";

export type StorageArea =
  | "outbound-queue"
  | "sm-resume"
  | "own-resume"
  | "catchup"
  | "dm-call-join-cache"
  | "muc-call-session-cache"
  | "call-token-cache";

export type XmppCondition =
  | "bad-format"
  | "bad-namespace-prefix"
  | "bad-request"
  | "conflict"
  | "connection-timeout"
  | "feature-not-implemented"
  | "forbidden"
  | "gone"
  | "host-gone"
  | "host-unknown"
  | "improper-addressing"
  | "internal-server-error"
  | "invalid-from"
  | "invalid-namespace"
  | "invalid-xml"
  | "item-not-found"
  | "jid-malformed"
  | "not-acceptable"
  | "not-allowed"
  | "not-authorized"
  | "not-well-formed"
  | "policy-violation"
  | "precondition-not-met"
  | "recipient-unavailable"
  | "redirect"
  | "registration-required"
  | "remote-connection-failed"
  | "remote-server-not-found"
  | "remote-server-timeout"
  | "reset"
  | "resource-constraint"
  | "restricted-xml"
  | "see-other-host"
  | "service-unavailable"
  | "subscription-required"
  | "system-shutdown"
  | "undefined-condition"
  | "unexpected-request"
  | "unknown"
  | "unsupported-encoding"
  | "unsupported-feature"
  | "unsupported-stanza-type"
  | "unsupported-version";

export type XmppStreamReason =
  | "stream-error"
  | "stream-handled-count-too-high"
  | "stream-transport-error"
  | "stream-invalid-transport-frame"
  | "stream-empty-transport-frame"
  | "stream-unsupported-websocket-message"
  | "stream-disconnected"
  | `stream-${XmppCondition}`
  | `disco-${"info" | "items" | "pubsub"}-${XmppCondition | "timeout"}`;

export type XmppOperationReason =
  | "reconnect-catchup-failed"
  | "missing-list-room-members"
  | "member-query-failed"
  | `member-query-${XmppCondition}`
  | "room-join-rejected"
  | `room-join-${XmppCondition}`;

type StorageFailureReason =
  | "read-failed"
  | "write-failed"
  | "owner-slot-limit"
  | "quota-exceeded"
  | "stale-entries-pruned";

export type TelemetryErrorObservation =
  | {
      kind: "xmpp.stream";
      reason: XmppStreamReason;
      recoverable: boolean;
      condition?: XmppCondition;
      errorType?: XmppStanzaErrorType;
      errorSource?: ErrorSource;
      sm?: { h: number; sendCount: number };
    }
  | {
      kind: "xmpp.auth" | "xmpp.disconnect";
      reason: "auth-error" | "connect-timeout" | "room-self-presence-timeout";
      recoverable: boolean;
      errorSource?: "local-timeout";
    }
  | {
      kind: "xmpp.history" | "xmpp.member-query" | "xmpp.muc-join";
      reason: XmppOperationReason;
      recoverable: boolean;
      condition?: XmppCondition;
      errorType?: XmppStanzaErrorType;
      errorSource?: ErrorSource;
    }
  | {
      kind: "storage.read" | "storage.write" | "storage.quota";
      reason: StorageFailureReason;
      area: StorageArea;
      dropped?: number;
      queueSize?: number;
    }
  | {
      kind: "http.fetch";
      operation: "auth-session-get" | "auth-logout-post";
      status: number;
      recoverable: boolean;
    }
  | {
      kind: "upload";
      operation: "xep-0363-put";
      failure: "http-status" | "network-error";
      status?: number;
    }
  | {
      kind: "call.operation";
      reason: "call-operation";
    }
  | {
      kind: "call.media";
      mediaKind: "mic" | "cam" | "screen";
      reason: "denied" | "missing" | "in-use" | "failed";
    }
  | {
      kind: "window-error" | "unhandled-rejection" | "vue-render-error";
      reason: "unexpected";
    };

export type TelemetrySpanObservation =
  | { kind: "xmpp-connect" }
  | { kind: "initial-render"; conversation: "dm" | "room" }
  | { kind: "room-switch" };

type XmppErrorContext = {
  kind: "xmpp.stream" | "xmpp.history" | "xmpp.member-query" | "xmpp.muc-join";
  reason: string;
  recoverable: string;
  condition?: string;
  errorType?: string;
  errorSource?: string;
  smH?: string;
  smSendCount?: string;
};

export type TelemetryErrorContext =
  | XmppErrorContext
  | {
      kind: "xmpp.auth" | "xmpp.disconnect";
      reason: string;
      recoverable: string;
      errorSource?: string;
    }
  | {
      kind: "storage.read" | "storage.write" | "storage.quota";
      reason: string;
      area: string;
      dropped?: string;
      queueSize?: string;
    }
  | { kind: "http.fetch"; operation: string; status: string; recoverable: string }
  | { kind: "upload"; operation: string; failure: string; status?: string }
  | { kind: "call.operation"; reason: string }
  | { kind: "call.media"; mediaKind: string; reason: string }
  | { kind: "window-error" | "unhandled-rejection" | "vue-render-error"; reason: string };

function canonicalError(message: string): Error {
  const error = new Error(message);
  error.stack = undefined;
  return error;
}

function boundedCounter(value: number): string {
  if (!Number.isFinite(value)) return "0";
  return String(Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, Math.floor(value))));
}

export function encodeTelemetryError(observation: TelemetryErrorObservation): {
  error: Error;
  type: TelemetryErrorObservation["kind"];
  context: TelemetryErrorContext;
} {
  switch (observation.kind) {
    case "xmpp.stream": {
      const context: XmppErrorContext = {
        kind: observation.kind,
        reason: observation.reason,
        recoverable: String(observation.recoverable),
      };
      if (observation.condition !== undefined) context.condition = observation.condition;
      if (observation.errorType !== undefined) context.errorType = observation.errorType;
      if (observation.errorSource !== undefined) context.errorSource = observation.errorSource;
      if (observation.sm !== undefined) {
        context.smH = boundedCounter(observation.sm.h);
        context.smSendCount = boundedCounter(observation.sm.sendCount);
      }
      return {
        error: canonicalError(`${observation.kind}.${observation.reason}`),
        type: observation.kind,
        context,
      };
    }
    case "xmpp.auth":
    case "xmpp.disconnect": {
      const context: Extract<TelemetryErrorContext, { kind: "xmpp.auth" | "xmpp.disconnect" }> = {
        kind: observation.kind,
        reason: observation.reason,
        recoverable: String(observation.recoverable),
      };
      if (observation.errorSource !== undefined) context.errorSource = observation.errorSource;
      return {
        error: canonicalError(`${observation.kind}.${observation.reason}`),
        type: observation.kind,
        context,
      };
    }
    case "xmpp.history":
    case "xmpp.member-query":
    case "xmpp.muc-join": {
      const context: XmppErrorContext = {
        kind: observation.kind,
        reason: observation.reason,
        recoverable: String(observation.recoverable),
      };
      if (observation.condition !== undefined) context.condition = observation.condition;
      if (observation.errorType !== undefined) context.errorType = observation.errorType;
      if (observation.errorSource !== undefined) context.errorSource = observation.errorSource;
      return {
        error: canonicalError(`${observation.kind}.${observation.reason}`),
        type: observation.kind,
        context,
      };
    }
    case "storage.read":
    case "storage.write":
    case "storage.quota": {
      const context: Extract<TelemetryErrorContext, { kind: "storage.read" | "storage.write" | "storage.quota" }> = {
        kind: observation.kind,
        reason: observation.reason,
        area: observation.area,
      };
      if (observation.dropped !== undefined) context.dropped = String(observation.dropped);
      if (observation.queueSize !== undefined) context.queueSize = String(observation.queueSize);
      return {
        error: canonicalError(`${observation.kind}.${observation.reason}`),
        type: observation.kind,
        context,
      };
    }
    case "http.fetch": {
      const context: Extract<TelemetryErrorContext, { kind: "http.fetch" }> = {
        kind: observation.kind,
        operation: observation.operation,
        status: String(observation.status),
        recoverable: String(observation.recoverable),
      };
      return {
        error: canonicalError(`${observation.kind}.${observation.operation}`),
        type: observation.kind,
        context,
      };
    }
    case "upload": {
      const context: Extract<TelemetryErrorContext, { kind: "upload" }> = {
        kind: observation.kind,
        operation: observation.operation,
        failure: observation.failure,
      };
      if (observation.status !== undefined) context.status = String(observation.status);
      return {
        error: canonicalError(`${observation.kind}.${observation.failure}`),
        type: observation.kind,
        context,
      };
    }
    case "call.operation":
      return {
        error: canonicalError(`${observation.kind}.${observation.reason}`),
        type: observation.kind,
        context: { kind: observation.kind, reason: observation.reason },
      };
    case "call.media":
      return {
        error: canonicalError(`${observation.kind}.${observation.mediaKind}.${observation.reason}`),
        type: observation.kind,
        context: {
          kind: observation.kind,
          mediaKind: observation.mediaKind,
          reason: observation.reason,
        },
      };
    case "window-error":
    case "unhandled-rejection":
    case "vue-render-error":
      return {
        error: canonicalError(`${observation.kind}.${observation.reason}`),
        type: observation.kind,
        context: { kind: observation.kind, reason: observation.reason },
      };
  }
}

type TelemetrySpanAttributes =
  | { "waddle.xmpp.transport": "websocket" }
  | { "conversation.kind": "dm" | "room" };

export function encodeTelemetrySpan(observation: TelemetrySpanObservation): {
  name: string;
  attributes: TelemetrySpanAttributes;
} {
  switch (observation.kind) {
    case "xmpp-connect":
      return { name: "xmpp.connect", attributes: { "waddle.xmpp.transport": "websocket" } };
    case "initial-render":
      return {
        name: "xmpp.initial_render",
        attributes: { "conversation.kind": observation.conversation },
      };
    case "room-switch":
      return { name: "xmpp.room_switch", attributes: { "conversation.kind": "room" } };
  }
}
