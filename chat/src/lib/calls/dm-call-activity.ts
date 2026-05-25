import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import { map } from "nanostores";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { CallEvent, CallMedia } from "./types";

export const DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS = 24 * 60 * 60 * 1000;
const MAX_TERMINAL_TIMESTAMPS = 1024;
const PRUNE_TIMER_SLACK_MS = 1_000;

type DmCallActivityState = "ringing" | "accepted";

export interface DmCallActivity {
  peerJid: string;
  sid: string;
  media: CallMedia;
  state: DmCallActivityState;
  direction: "incoming" | "outgoing" | "unknown";
  updatedAt: string;
}

export const $dmCallActivities = map<Record<string, DmCallActivity>>({});
const dmCallTerminalTimestamps = new Map<string, string>();
let pruneTimer: ReturnType<typeof setTimeout> | null = null;

interface DmCallEventEnvelope {
  event: CallEvent;
  selfBareJid: string;
  to?: string | null;
  timestamp?: string | null;
  now?: Date;
  directionHint?: DmCallActivity["direction"];
}

function normalizedBare(jid?: string | null): string {
  return barePeerJid(jid ?? "").toLowerCase();
}

function eventMedia(event: CallEvent, previous?: DmCallActivity): CallMedia {
  if ("media" in event) return event.media;
  return previous?.media ?? { audio: true, video: false };
}

function timestampFromEnvelope(envelope: DmCallEventEnvelope): string {
  return envelope.timestamp || envelope.now?.toISOString() || new Date().toISOString();
}

function isStale(timestamp: string, now: Date): boolean {
  const ms = Date.parse(timestamp);
  if (!Number.isFinite(ms)) return false;
  return now.getTime() - ms > DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS;
}

function millisecondsUntilStale(timestamp: string, now: Date): number | null {
  const ms = Date.parse(timestamp);
  if (!Number.isFinite(ms)) return null;
  return ms + DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS - now.getTime();
}

function isOlderThanCurrent(timestamp: string, current?: DmCallActivity): boolean {
  if (!current) return false;
  const nextMs = Date.parse(timestamp);
  const currentMs = Date.parse(current.updatedAt);
  if (!Number.isFinite(nextMs) || !Number.isFinite(currentMs)) return false;
  return nextMs < currentMs;
}

function terminalKey(peerJid: string, sid: string): string {
  return `${peerJid}\u0000${sid}`;
}

function isTerminalEvent(event: CallEvent): boolean {
  return event.kind === "reject" ||
    event.kind === "retract" ||
    event.kind === "finish" ||
    event.kind === "session-terminate";
}

function isOlderThanTerminal(peerJid: string, sid: string, timestamp: string): boolean {
  const terminalTimestamp = dmCallTerminalTimestamps.get(terminalKey(peerJid, sid));
  if (!terminalTimestamp) return false;
  const nextMs = Date.parse(timestamp);
  const terminalMs = Date.parse(terminalTimestamp);
  if (!Number.isFinite(nextMs) || !Number.isFinite(terminalMs)) return false;
  return nextMs <= terminalMs;
}

function pruneTerminalTimestamps(now: Date): void {
  for (const [key, timestamp] of dmCallTerminalTimestamps) {
    if (isStale(timestamp, now)) {
      dmCallTerminalTimestamps.delete(key);
    }
  }
  while (dmCallTerminalTimestamps.size > MAX_TERMINAL_TIMESTAMPS) {
    const oldest = dmCallTerminalTimestamps.keys().next().value;
    if (!oldest) break;
    dmCallTerminalTimestamps.delete(oldest);
  }
}

function clearPruneTimer(): void {
  if (!pruneTimer) return;
  clearTimeout(pruneTimer);
  pruneTimer = null;
}

function scheduleActivityPrune(now = new Date()): void {
  clearPruneTimer();
  let nextDelay: number | null = null;
  for (const activity of Object.values($dmCallActivities.get())) {
    const delay = millisecondsUntilStale(activity.updatedAt, now);
    if (delay === null) continue;
    nextDelay = nextDelay === null ? delay : Math.min(nextDelay, delay);
  }
  if (nextDelay === null) return;
  pruneTimer = setTimeout(() => {
    pruneTimer = null;
    pruneExpiredDmCallActivities();
  }, Math.max(0, nextDelay) + PRUNE_TIMER_SLACK_MS);
  (pruneTimer as { unref?: () => void }).unref?.();
}

export function pruneExpiredDmCallActivities(now = new Date()): void {
  const current = $dmCallActivities.get();
  const next: Record<string, DmCallActivity> = {};
  let changed = false;
  for (const [peerJid, activity] of Object.entries(current)) {
    if (isStale(activity.updatedAt, now)) {
      recordTerminal(peerJid, activity.sid, activity.updatedAt, now);
      changed = true;
      continue;
    }
    next[peerJid] = activity;
  }
  pruneTerminalTimestamps(now);
  if (changed) {
    $dmCallActivities.set(next);
  }
  scheduleActivityPrune(now);
}

function recordTerminal(peerJid: string, sid: string, timestamp: string, now = new Date()): void {
  const key = terminalKey(peerJid, sid);
  const previous = dmCallTerminalTimestamps.get(key);
  if (previous && isOlderThanTerminal(peerJid, sid, timestamp)) return;
  dmCallTerminalTimestamps.set(key, timestamp);
  pruneTerminalTimestamps(now);
}

function peerForSid(sid: string): string {
  for (const [peerJid, activity] of Object.entries($dmCallActivities.get())) {
    if (activity.sid === sid) return peerJid;
  }
  return "";
}

function peerForEnvelope(envelope: DmCallEventEnvelope): string {
  const selfBare = normalizedBare(envelope.selfBareJid);
  const fromBare = normalizedBare(envelope.event.from);
  const toBare = normalizedBare(envelope.to ?? envelope.event.to);
  if (fromBare && fromBare !== selfBare) return fromBare;
  if (toBare && toBare !== selfBare) return toBare;
  if (fromBare && fromBare === selfBare) return peerForSid(envelope.event.sid);
  return "";
}

function directionForEnvelope(envelope: DmCallEventEnvelope): DmCallActivity["direction"] {
  if (envelope.directionHint) return envelope.directionHint;
  const selfBare = normalizedBare(envelope.selfBareJid);
  const fromBare = normalizedBare(envelope.event.from);
  if (!selfBare || !fromBare) return "unknown";
  return fromBare === selfBare ? "outgoing" : "incoming";
}

function removeActivity(peerJid: string, sid: string): void {
  const current = $dmCallActivities.get()[peerJid];
  if (!current || current.sid !== sid) return;
  const next = { ...$dmCallActivities.get() };
  delete next[peerJid];
  $dmCallActivities.set(next);
  scheduleActivityPrune();
}

export function clearDmCallActivity(peerJid: string, sid?: string): void {
  const normalized = normalizedBare(peerJid);
  if (!normalized) return;
  const current = $dmCallActivities.get()[normalized];
  const now = new Date();
  if (sid) recordTerminal(normalized, sid, now.toISOString(), now);
  if (!current) return;
  if (sid && current.sid !== sid) return;
  recordTerminal(normalized, current.sid, now.toISOString(), now);
  const next = { ...$dmCallActivities.get() };
  delete next[normalized];
  $dmCallActivities.set(next);
}

export function applyDmCallEvent(envelope: DmCallEventEnvelope): void {
  const peerJid = peerForEnvelope(envelope);
  if (!peerJid) return;
  const timestamp = timestampFromEnvelope(envelope);
  const now = envelope.now ?? new Date();
  if (isStale(timestamp, now)) {
    removeActivity(peerJid, envelope.event.sid);
    return;
  }
  const previous = $dmCallActivities.get()[peerJid];
  if (isOlderThanCurrent(timestamp, previous)) return;
  if (!isTerminalEvent(envelope.event) && isOlderThanTerminal(peerJid, envelope.event.sid, timestamp)) return;
  switch (envelope.event.kind) {
    case "propose":
      $dmCallActivities.setKey(peerJid, {
        peerJid,
        sid: envelope.event.sid,
        media: envelope.event.media,
        state: "ringing",
        direction: directionForEnvelope(envelope),
        updatedAt: timestamp,
      });
      scheduleActivityPrune(now);
      return;
    case "proceed":
    case "session-initiate":
    case "session-accept":
      $dmCallActivities.setKey(peerJid, {
        peerJid,
        sid: envelope.event.sid,
        media: eventMedia(envelope.event, previous),
        state: "accepted",
        direction: previous?.direction ?? directionForEnvelope(envelope),
        updatedAt: timestamp,
      });
      scheduleActivityPrune(now);
      return;
    case "reject":
    case "retract":
    case "finish":
    case "session-terminate":
      recordTerminal(peerJid, envelope.event.sid, timestamp, now);
      removeActivity(peerJid, envelope.event.sid);
      return;
  }
}

export function clearDmCallActivities(): void {
  clearPruneTimer();
  $dmCallActivities.set({});
  dmCallTerminalTimestamps.clear();
}

export function readDmCallActivity(peerJid: string, now = new Date()): DmCallActivity | null {
  pruneExpiredDmCallActivities(now);
  const normalized = normalizedBare(peerJid);
  if (!normalized) return null;
  return $dmCallActivities.get()[normalized] ?? null;
}

export function useDmCallActivity(
  peerJid: () => string | null | undefined,
): {
  activity: ComputedRef<DmCallActivity | null>;
  hasActivity: ComputedRef<boolean>;
} {
  const activities = useStore($dmCallActivities);
  const activity = computed<DmCallActivity | null>(() => {
    const normalized = normalizedBare(peerJid());
    if (!normalized) return null;
    return activities.value[normalized] ?? null;
  });
  const hasActivity = computed<boolean>(() => activity.value !== null);
  return { activity, hasActivity };
}
