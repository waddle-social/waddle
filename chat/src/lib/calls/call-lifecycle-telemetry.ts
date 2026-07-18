import {
  type CallConnectionPhase,
  type CallConnectionQuality,
} from "./connection-quality";
import { reportCallLifecycle } from "../telemetry";

export type CallKind = "dm" | "muc";
export type CallSetupOutcome = "proposed" | "accepted" | "declined" | "timeout" | "failed";
export type CallEndReason = "hangup" | "peer-left" | "error" | "reconnect-exhausted";
export type CallDurationBucket = "none" | "under-1m" | "1m-10m" | "10m-60m" | "over-60m";
export type CallRttBand = "unknown" | "under-100ms" | "100ms-300ms" | "over-300ms";
export type CallPacketLossBand = "unknown" | "under-1pct" | "1pct-5pct" | "over-5pct";
export type CallReconnectCountBucket = "none" | "once" | "multiple";

export type CallLifecyclePayload = {
  setupOutcome: CallSetupOutcome;
  endReason: CallEndReason;
  durationBucket: CallDurationBucket;
  callKind: CallKind;
  rttBand: CallRttBand;
  packetLossBand: CallPacketLossBand;
  connectionQuality: CallConnectionQuality;
  reconnectCount: CallReconnectCountBucket;
};

type Attempt = {
  sid: string;
  callKind: CallKind;
  accepted: boolean;
  acceptedAtMs: number | null;
  maxRttMs: number | null;
  maxPacketLossPct: number | null;
  worstConnectionQuality: CallConnectionQuality;
  reconnectCount: number;
  previousConnectionPhase: CallConnectionPhase;
  reconnectInProgress: boolean;
};

const QUALITY_SEVERITY: Record<CallConnectionQuality, number> = {
  unknown: 0,
  excellent: 1,
  good: 2,
  poor: 3,
  lost: 4,
};

const attempts = new Map<string, Attempt>();
// Insertion-ordered dedupe window: exactly-once per sid holds for the
// most recent MAX_EMITTED_SIDS call attempts. Bounded so a hostile
// stream of inbound proposals cannot grow memory for the tab's
// lifetime; a sid old enough to be evicted re-emitting is acceptable.
const MAX_EMITTED_SIDS = 4096;
const emittedSids = new Set<string>();
let currentSid: string | null = null;

function markEmitted(sid: string): void {
  emittedSids.add(sid);
  if (emittedSids.size <= MAX_EMITTED_SIDS) return;
  for (const oldest of emittedSids) {
    emittedSids.delete(oldest);
    if (emittedSids.size <= MAX_EMITTED_SIDS) break;
  }
}

export function durationBucket(durationMs: number): CallDurationBucket {
  if (durationMs <= 0) return "none";
  if (durationMs < 60_000) return "under-1m";
  if (durationMs < 600_000) return "1m-10m";
  if (durationMs < 3_600_000) return "10m-60m";
  return "over-60m";
}

export function rttBand(rttMs: number | null): CallRttBand {
  if (rttMs === null) return "unknown";
  if (rttMs < 100) return "under-100ms";
  if (rttMs <= 300) return "100ms-300ms";
  return "over-300ms";
}

export function packetLossBand(packetLossPct: number | null): CallPacketLossBand {
  if (packetLossPct === null) return "unknown";
  if (packetLossPct < 1) return "under-1pct";
  if (packetLossPct <= 5) return "1pct-5pct";
  return "over-5pct";
}

export function reconnectCountBucket(count: number): CallReconnectCountBucket {
  if (count <= 0) return "none";
  if (count === 1) return "once";
  return "multiple";
}

export function beginCallAttempt(sid: string, callKind: CallKind): void {
  if (!sid) return;
  if (emittedSids.has(sid)) return;
  if (currentSid && currentSid !== sid) {
    const current = attempts.get(currentSid);
    finishCallAttempt(currentSid, {
      setupOutcome: "proposed",
      endReason: current?.accepted ? "error" : "hangup",
    });
  }
  if (!attempts.has(sid)) {
    attempts.set(sid, {
      sid,
      callKind,
      accepted: false,
      acceptedAtMs: null,
      maxRttMs: null,
      maxPacketLossPct: null,
      worstConnectionQuality: "unknown",
      reconnectCount: 0,
      previousConnectionPhase: "disconnected",
      reconnectInProgress: false,
    });
  }
  currentSid = sid;
}

/** Classify a terminal media transport loss using observed reconnect state. */
export function finishCallAttemptForTransportDisconnect(sid: string): CallLifecyclePayload | null {
  const attempt = attempts.get(sid);
  return finishCallAttempt(sid, {
    setupOutcome: "accepted",
    endReason: attempt?.reconnectInProgress ? "reconnect-exhausted" : "error",
  });
}

export function markCallAttemptAccepted(sid: string, now: number = Date.now()): void {
  const attempt = attempts.get(sid);
  if (attempt) {
    attempt.accepted = true;
    attempt.acceptedAtMs ??= now;
  }
}

export function observeCallStats(sample: {
  rttMs: number | null;
  packetLossPct: number | null;
}): void {
  const attempt = currentAttempt();
  if (!attempt) return;
  if (sample.rttMs !== null) {
    attempt.maxRttMs = Math.max(attempt.maxRttMs ?? 0, sample.rttMs);
  }
  if (sample.packetLossPct !== null) {
    attempt.maxPacketLossPct = Math.max(attempt.maxPacketLossPct ?? 0, sample.packetLossPct);
  }
}

export function observeCallConnectionQuality(quality: CallConnectionQuality): void {
  const attempt = currentAttempt();
  if (!attempt) return;
  if (QUALITY_SEVERITY[quality] > QUALITY_SEVERITY[attempt.worstConnectionQuality]) {
    attempt.worstConnectionQuality = quality;
  }
}

export function observeCallConnectionPhase(phase: CallConnectionPhase): void {
  const attempt = currentAttempt();
  if (!attempt) return;
  if (phase === "reconnecting" && attempt.previousConnectionPhase !== "reconnecting") {
    attempt.reconnectCount += 1;
  }
  if (phase === "reconnecting") attempt.reconnectInProgress = true;
  if (phase === "connected") attempt.reconnectInProgress = false;
  attempt.previousConnectionPhase = phase;
}

export function finishCallAttempt(
  sid: string,
  terminal: { setupOutcome?: CallSetupOutcome; endReason: CallEndReason },
  now: number = Date.now(),
): CallLifecyclePayload | null {
  if (emittedSids.has(sid)) return null;
  const attempt = attempts.get(sid);
  if (!attempt) return null;
  const payload: CallLifecyclePayload = {
    setupOutcome: attempt.accepted ? "accepted" : (terminal.setupOutcome ?? "proposed"),
    endReason: terminal.endReason,
    durationBucket: durationBucket(attempt.acceptedAtMs === null ? 0 : now - attempt.acceptedAtMs),
    callKind: attempt.callKind,
    rttBand: rttBand(attempt.maxRttMs),
    packetLossBand: packetLossBand(attempt.maxPacketLossPct),
    connectionQuality: attempt.worstConnectionQuality,
    reconnectCount: reconnectCountBucket(attempt.reconnectCount),
  };
  markEmitted(sid);
  attempts.delete(sid);
  if (currentSid === sid) currentSid = null;
  reportCallLifecycle(payload);
  return payload;
}

/** Emit a declined inbound proposal that never occupied the single call slot. */
export function reportDeclinedCallAttempt(sid: string, callKind: CallKind = "dm"): void {
  if (!sid || attempts.has(sid) || emittedSids.has(sid)) return;
  markEmitted(sid);
  reportCallLifecycle({
    setupOutcome: "declined",
    endReason: "hangup",
    durationBucket: "none",
    callKind,
    rttBand: "unknown",
    packetLossBand: "unknown",
    connectionQuality: "unknown",
    reconnectCount: "none",
  });
}

/** Emit a failed setup that ended before it could occupy the call store. */
export function reportFailedCallAttempt(sid: string, callKind: CallKind): void {
  if (!sid || attempts.has(sid) || emittedSids.has(sid)) return;
  markEmitted(sid);
  reportCallLifecycle({
    setupOutcome: "failed",
    endReason: "error",
    durationBucket: "none",
    callKind,
    rttBand: "unknown",
    packetLossBand: "unknown",
    connectionQuality: "unknown",
    reconnectCount: "none",
  });
}

function currentAttempt(): Attempt | undefined {
  return currentSid ? attempts.get(currentSid) : undefined;
}

/** Test seam: lifecycle state must not leak across Bun test cases. */
export function __resetCallLifecycleTelemetryForTesting(): void {
  attempts.clear();
  emittedSids.clear();
  currentSid = null;
}
