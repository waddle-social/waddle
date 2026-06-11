import { computed, type ComputedRef } from "vue";
import { useStore } from "@nanostores/vue";
import { map } from "nanostores";
import { barePeerJid } from "@/lib/xmpp/jid";
import {
  forgetDmCallJoin,
  readDmCallJoin,
  rememberDmCallJoin,
} from "./dm-call-join-cache";
import {
  publishDmCallOutcomeAnchor,
  type DmCallOutcome,
  type DmCallOutcomeAnchor,
} from "./dm-call-anchor";
import type { CallEvent, CallMedia, LiveKitJoin } from "./types";

export const DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS = 24 * 60 * 60 * 1000;
const MAX_TERMINAL_TIMESTAMPS = 1024;
const PRUNE_TIMER_SLACK_MS = 1_000;
export const LIVEKIT_JOIN_EXPIRY_SKEW_MS = 30_000;

type DmCallActivityState = "ringing" | "accepted";
type DmCallResumeBlockReason =
  | "missing-self"
  | "not-accepted"
  | "unknown-media"
  | "missing-peer-resource"
  | "missing-join"
  | "other-resource"
  | "invalid-token"
  | "expired-token";
type DmCallEndBlockReason =
  | "missing-self"
  | "not-accepted"
  | "missing-peer-resource"
  | "missing-join"
  | "other-resource";

export interface DmCallActivity {
  peerJid: string;
  /** Full JID used for XEP-0353 responses when an incoming proposal
   * is restored from MAM after refresh. */
  remoteFullJid?: string;
  sid: string;
  media: CallMedia;
  mediaKnown?: boolean;
  /**
   * LiveKit credentials recovered from an archived Jingle
   * session-initiate/session-accept. A resume path may use this only
   * after checking that the token identity matches the current full
   * JID; otherwise another browser/resource could consume the wrong
   * participant identity.
   */
  join?: LiveKitJoin;
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
  selfFullJid?: string | null;
  to?: string | null;
  timestamp?: string | null;
  now?: Date;
  directionHint?: DmCallActivity["direction"];
  publishOutcome?: boolean;
}

function normalizedBare(jid?: string | null): string {
  return barePeerJid(jid ?? "").toLowerCase();
}

export function hasKnownDmCallMedia(
  activity: Pick<DmCallActivity, "mediaKnown">,
): boolean {
  return activity.mediaKnown !== false;
}

export function canResumeDmCallActivity(
  activity: Pick<DmCallActivity, "state" | "mediaKnown" | "remoteFullJid" | "join">,
  selfFullJid: string | null | undefined,
  now = new Date(),
): activity is Pick<DmCallActivity, "state" | "mediaKnown" | "remoteFullJid" | "join"> & {
  remoteFullJid: string;
  join: LiveKitJoin;
} {
  return dmCallResumeBlockReason(activity, selfFullJid, now) === null;
}

export function canEndRecoveredDmCallActivity(
  activity: Pick<DmCallActivity, "state" | "remoteFullJid" | "join">,
  selfFullJid: string | null | undefined,
): activity is Pick<DmCallActivity, "state" | "remoteFullJid" | "join"> & {
  remoteFullJid: string;
  join: LiveKitJoin;
} {
  return dmCallEndBlockReason(activity, selfFullJid) === null;
}

function dmCallEndBlockReason(
  activity: Pick<DmCallActivity, "state" | "remoteFullJid" | "join">,
  selfFullJid: string | null | undefined,
): DmCallEndBlockReason | null {
  const self = selfFullJid?.trim();
  if (!self) return "missing-self";
  if (activity.state !== "accepted") return "not-accepted";
  if (!activity.remoteFullJid) return "missing-peer-resource";
  if (!activity.join) return "missing-join";
  if (activity.join.identity !== self) return "other-resource";
  return null;
}

export function dmCallResumeBlockReason(
  activity: Pick<DmCallActivity, "state" | "mediaKnown" | "remoteFullJid" | "join">,
  selfFullJid: string | null | undefined,
  now = new Date(),
): DmCallResumeBlockReason | null {
  const self = selfFullJid?.trim();
  if (!self) return "missing-self";
  if (activity.state !== "accepted") return "not-accepted";
  if (!hasKnownDmCallMedia(activity)) return "unknown-media";
  if (!activity.remoteFullJid) return "missing-peer-resource";
  if (!activity.join) return "missing-join";
  if (activity.join.identity !== self) return "other-resource";
  const expiresAt = liveKitJoinTokenExpiresAt(activity.join.token);
  if (expiresAt === null) return "invalid-token";
  if (expiresAt.getTime() <= now.getTime() + LIVEKIT_JOIN_EXPIRY_SKEW_MS) return "expired-token";
  return null;
}

function eventMedia(
  event: CallEvent,
  previous?: DmCallActivity,
): { media: CallMedia; known: boolean } {
  if ("media" in event) return { media: event.media, known: true };
  if (previous) return { media: previous.media, known: hasKnownDmCallMedia(previous) };
  return { media: { audio: true, video: false }, known: false };
}

function eventJoin(event: CallEvent, previous?: DmCallActivity): LiveKitJoin | undefined {
  if ("join" in event) return event.join;
  return previous?.sid === event.sid ? previous.join : undefined;
}

function eventSelfFullJid(envelope: DmCallEventEnvelope, join?: LiveKitJoin): string | undefined {
  const selfBare = normalizedBare(envelope.selfBareJid);
  const candidates = [
    join?.identity,
    envelope.selfFullJid,
    envelope.event.from,
    envelope.to ?? envelope.event.to,
  ];
  for (const candidate of candidates) {
    if (!candidate || !candidate.includes("/")) continue;
    if (normalizedBare(candidate) === selfBare) return candidate.trim();
  }
  return undefined;
}

/**
 * Decode a LiveKit join JWT's payload (the middle segment) into a
 * plain object, or `null` for malformed/undecodable input. Shared by
 * the expiry check and the video-grant validation so both parse the
 * token exactly once per concern without duplicating the base64url +
 * JSON dance.
 */
function decodeLiveKitJwtPayload(token: string): Record<string, unknown> | null {
  const payload = token.split(".")[1];
  if (!payload) return null;
  try {
    const parsed = JSON.parse(decodeBase64UrlUtf8(payload)) as unknown;
    if (!parsed || typeof parsed !== "object") return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

/**
 * Decode a LiveKit join JWT and return its `exp` claim as a `Date`,
 * or `null` for malformed input. Exported so the MUC resume path
 * (`resumeMucCallActivity`) can reuse the same expiry check the DM
 * resume path uses without re-implementing JWT decoding.
 */
export function liveKitJoinTokenExpiresAt(token: string): Date | null {
  const parsed = decodeLiveKitJwtPayload(token);
  if (!parsed) return null;
  const exp = Number(parsed.exp);
  if (!Number.isFinite(exp) || exp <= 0) return null;
  return new Date(exp * 1000);
}

/**
 * Outcome of validating a LiveKit join token's `video` grant before
 * we hand it to the SDK. `ok` is the only success; every failure
 * names the specific defect so the call surface can surface a clear
 * error instead of the SDK's opaque late rejection.
 */
type LiveKitGrantValidation =
  | { ok: true }
  | { ok: false; reason: "malformed-token" | "missing-grant" | "join-not-granted" | "missing-room" };

/**
 * Defensively validate the `video` grant encoded in a LiveKit join
 * JWT before `Room.connect`. The server mints camelCase grant fields
 * (`roomJoin`, `room`, `canPublish`, `canSubscribe`,
 * `canPublishData`); at minimum a usable join token must grant
 * `roomJoin === true` for a non-empty `room`. Failing fast here turns
 * a misconfigured / mismatched token into a clear local error rather
 * than letting the SDK reject the WebSocket upgrade opaquely.
 *
 * Pure: no I/O, no SDK dependency, so it's trivially unit-testable
 * alongside the other token helpers.
 */
export function validateLiveKitGrant(token: string): LiveKitGrantValidation {
  const parsed = decodeLiveKitJwtPayload(token);
  if (!parsed) return { ok: false, reason: "malformed-token" };
  const grant = parsed.video;
  if (!grant || typeof grant !== "object") return { ok: false, reason: "missing-grant" };
  const video = grant as Record<string, unknown>;
  if (video.roomJoin !== true) return { ok: false, reason: "join-not-granted" };
  if (typeof video.room !== "string" || video.room.trim() === "") {
    return { ok: false, reason: "missing-room" };
  }
  return { ok: true };
}

function decodeBase64UrlUtf8(value: string): string {
  const binary = globalThis.atob(normalizeBase64Url(value));
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

function normalizeBase64Url(value: string): string {
  const normalized = value.replaceAll("-", "+").replaceAll("_", "/");
  const padding = (4 - (normalized.length % 4)) % 4;
  return `${normalized}${"=".repeat(padding)}`;
}

function timestampFromEnvelope(envelope: DmCallEventEnvelope): string {
  return envelope.timestamp || envelope.now?.toISOString() || new Date().toISOString();
}

function isStale(timestamp: string, now: Date): boolean {
  const ms = Date.parse(timestamp);
  if (!Number.isFinite(ms)) return true;
  return now.getTime() - ms > DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS;
}

function millisecondsUntilStale(timestamp: string, now: Date): number {
  const ms = Date.parse(timestamp);
  if (!Number.isFinite(ms)) return 0;
  return ms + DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS - now.getTime();
}

function millisecondsUntilResumeAffordanceChange(activity: DmCallActivity, now: Date): number | null {
  if (activity.state !== "accepted" || !activity.join) return null;
  const expiresAt = liveKitJoinTokenExpiresAt(activity.join.token);
  if (!expiresAt) return null;
  const delay = expiresAt.getTime() - LIVEKIT_JOIN_EXPIRY_SKEW_MS - now.getTime();
  return delay > 0 ? delay : null;
}

function isOlderThanCurrent(timestamp: string, current?: DmCallActivity): boolean {
  if (!current) return false;
  const nextMs = Date.parse(timestamp);
  const currentMs = Date.parse(current.updatedAt);
  if (!Number.isFinite(nextMs) || !Number.isFinite(currentMs)) return false;
  return nextMs < currentMs;
}

function activityStorageKey(peerJid: string, sid: string, selfFullJid?: string | null): string {
  const normalized = normalizedBare(peerJid);
  const self = selfFullJid?.trim();
  return self ? `${normalized}\u0000${sid}\u0000${self}` : `${normalized}\u0000${sid}`;
}

function setActivity(
  peerJid: string,
  sid: string,
  selfFullJid: string | null | undefined,
  activity: DmCallActivity,
): void {
  const key = activityStorageKey(peerJid, sid, selfFullJid);
  const next = { ...$dmCallActivities.get() };
  for (const [existingKey, existing] of Object.entries(next)) {
    if (normalizedBare(existing.peerJid) === normalizedBare(peerJid) && existing.sid === sid && existingKey !== key) {
      delete next[existingKey];
    }
  }
  next[key] = activity;
  $dmCallActivities.set(next);
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

function outcomeForTerminalEvent(
  event: CallEvent,
  previous: DmCallActivity | undefined,
  direction: DmCallActivity["direction"],
): DmCallOutcome {
  if (event.kind === "reject") return "declined";
  if (event.kind === "session-terminate" && event.reason === "timeout") return "no-answer";
  if (event.kind === "finish" || event.kind === "session-terminate") return "ended";
  const callDirection = previous?.direction ?? direction;
  return callDirection === "outgoing" ? "no-answer" : "missed";
}

function terminalOutcomeAnchor(params: {
  peerJid: string;
  sid: string;
  timestamp: string;
  event: CallEvent;
  previous?: DmCallActivity;
  direction: DmCallActivity["direction"];
  selfJid?: string | null;
}): DmCallOutcomeAnchor {
  const media = eventMedia(params.event, params.previous);
  const initiator = terminalOutcomeInitiator(params);
  return {
    peerBareJid: params.peerJid,
    sid: params.sid,
    media: media.media,
    outcome: outcomeForTerminalEvent(params.event, params.previous, params.direction),
    ...(initiator ? { initiator } : {}),
    ended: params.timestamp,
  };
}

function terminalOutcomeInitiator(params: {
  peerJid: string;
  event: CallEvent;
  previous?: DmCallActivity;
  direction: DmCallActivity["direction"];
  selfJid?: string | null;
}): string | undefined {
  if (params.previous?.direction === "outgoing") return params.selfJid ?? undefined;
  if (params.previous?.direction === "incoming") return params.previous.remoteFullJid ?? params.event.from;
  const selfBare = normalizedBare(params.selfJid);
  const fromBare = normalizedBare(params.event.from);
  if (params.event.kind === "reject") {
    return fromBare === selfBare ? params.peerJid : params.selfJid ?? undefined;
  }
  if (params.event.kind === "retract") return params.event.from;
  if (params.direction === "outgoing") return params.selfJid ?? undefined;
  if (params.direction === "incoming") return params.event.from;
  return undefined;
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
    nextDelay = nextDelay === null ? delay : Math.min(nextDelay, delay);
    const resumeDelay = millisecondsUntilResumeAffordanceChange(activity, now);
    if (resumeDelay !== null) {
      nextDelay = nextDelay === null ? resumeDelay : Math.min(nextDelay, resumeDelay);
    }
  }
  if (nextDelay === null) return;
  pruneTimer = setTimeout(() => {
    pruneTimer = null;
    const before = $dmCallActivities.get();
    pruneExpiredDmCallActivities();
    if ($dmCallActivities.get() === before) {
      refreshDmCallActivityAffordances();
    }
  }, Math.max(0, nextDelay) + PRUNE_TIMER_SLACK_MS);
  (pruneTimer as { unref?: () => void }).unref?.();
}

export function refreshDmCallActivityAffordances(): void {
  $dmCallActivities.set({ ...$dmCallActivities.get() });
}

export function pruneExpiredDmCallActivities(now = new Date()): void {
  const current = $dmCallActivities.get();
  const next: Record<string, DmCallActivity> = {};
  let changed = false;
  for (const [key, activity] of Object.entries(current)) {
    const peerJid = normalizedBare(activity.peerJid);
    if (isStale(activity.updatedAt, now)) {
      recordTerminal(peerJid, activity.sid, activity.updatedAt, now);
      changed = true;
      continue;
    }
    next[key] = activity;
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
  for (const activity of Object.values($dmCallActivities.get())) {
    if (activity.sid === sid) return normalizedBare(activity.peerJid);
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

function remoteFullJidForEnvelope(
  envelope: DmCallEventEnvelope,
  peerJid: string,
): string | undefined {
  const candidates = [
    envelope.event.from,
    envelope.to ?? envelope.event.to,
  ];
  for (const candidate of candidates) {
    if (!candidate || !candidate.includes("/")) continue;
    if (normalizedBare(candidate) === peerJid) return candidate;
  }
  return undefined;
}

function dmCallActivityEntriesForPeer(
  activities: Record<string, DmCallActivity>,
  peerJid: string,
): Array<[string, DmCallActivity]> {
  const normalized = normalizedBare(peerJid);
  if (!normalized) return [];
  return Object.entries(activities)
    .filter(([, activity]) => normalizedBare(activity.peerJid) === normalized);
}

export function dmCallActivitiesForPeer(
  activities: Record<string, DmCallActivity>,
  peerJid: string,
  selfFullJid?: string | null,
): DmCallActivity[] {
  const self = selfFullJid?.trim();
  return dmCallActivityEntriesForPeer(activities, peerJid)
    .map(([, activity]) => activity)
    .sort((left, right) => compareDmCallActivityPriority(left, right, self));
}

function compareDmCallActivityPriority(
  left: DmCallActivity,
  right: DmCallActivity,
  selfFullJid?: string,
): number {
  if (selfFullJid) {
    const leftOwnResource = isOwnAcceptedResourceActivity(left, selfFullJid);
    const rightOwnResource = isOwnAcceptedResourceActivity(right, selfFullJid);
    if (leftOwnResource !== rightOwnResource) return leftOwnResource ? -1 : 1;
  }
  const rightMs = Date.parse(right.updatedAt);
  const leftMs = Date.parse(left.updatedAt);
  if (Number.isFinite(rightMs) && Number.isFinite(leftMs) && rightMs !== leftMs) {
    return rightMs - leftMs;
  }
  return right.sid.localeCompare(left.sid);
}

function isOwnAcceptedResourceActivity(activity: DmCallActivity, selfFullJid: string): boolean {
  return activity.state === "accepted" &&
    activity.join?.identity === selfFullJid &&
    Boolean(activity.remoteFullJid);
}

function findActivityForSid(peerJid: string, sid: string): [string, DmCallActivity] | null {
  return dmCallActivityEntriesForPeer($dmCallActivities.get(), peerJid)
    .find(([, activity]) => activity.sid === sid) ?? null;
}

function bestActivityForPeer(
  peerJid: string,
  selfFullJid?: string | null,
): DmCallActivity | null {
  const entries = dmCallActivitiesForPeer($dmCallActivities.get(), peerJid, selfFullJid);
  if (entries.length === 0) return null;
  return entries[0] ?? null;
}

function removeActivity(peerJid: string, sid: string): void {
  const entries = dmCallActivityEntriesForPeer($dmCallActivities.get(), peerJid)
    .filter(([, activity]) => activity.sid === sid);
  if (entries.length === 0) return;
  const next = { ...$dmCallActivities.get() };
  for (const [key, current] of entries) {
    const selfBareJid = current.join ? normalizedBare(current.join.identity) : "";
    if (selfBareJid) forgetDmCallJoin({ selfBareJid, peerJid: normalizedBare(current.peerJid), sid });
    delete next[key];
  }
  $dmCallActivities.set(next);
  scheduleActivityPrune();
}

export function clearDmCallActivity(peerJid: string, sid?: string): void {
  const normalized = normalizedBare(peerJid);
  if (!normalized) return;
  const currentEntries = dmCallActivityEntriesForPeer($dmCallActivities.get(), normalized);
  const now = new Date();
  if (sid) recordTerminal(normalized, sid, now.toISOString(), now);
  const next = { ...$dmCallActivities.get() };
  for (const [key, current] of currentEntries) {
    if (sid && current.sid !== sid) continue;
    recordTerminal(normalized, current.sid, now.toISOString(), now);
    const selfBareJid = current.join ? normalizedBare(current.join.identity) : "";
    if (selfBareJid) forgetDmCallJoin({ selfBareJid, peerJid: normalized, sid: current.sid });
    delete next[key];
  }
  $dmCallActivities.set(next);
}

export function applyDmCallEvent(envelope: DmCallEventEnvelope): DmCallOutcomeAnchor | null {
  const peerJid = peerForEnvelope(envelope);
  if (!peerJid) return null;
  const timestamp = timestampFromEnvelope(envelope);
  const now = envelope.now ?? new Date();
  if (isStale(timestamp, now)) {
    removeActivity(peerJid, envelope.event.sid);
    return null;
  }
  const previousForSidEntry = findActivityForSid(peerJid, envelope.event.sid);
  const previousForSid = previousForSidEntry?.[1];
  if (isOlderThanCurrent(timestamp, previousForSid)) return null;
  if (!isTerminalEvent(envelope.event) && isOlderThanTerminal(peerJid, envelope.event.sid, timestamp)) return null;
  const direction = directionForEnvelope(envelope);
  const remoteFullJid = remoteFullJidForEnvelope(envelope, peerJid);
  switch (envelope.event.kind) {
    case "ringing":
      return null;
    case "propose":
      setActivity(peerJid, envelope.event.sid, eventSelfFullJid(envelope), {
        peerJid,
        ...(remoteFullJid ? { remoteFullJid } : {}),
        sid: envelope.event.sid,
        media: envelope.event.media,
        state: "ringing",
        direction,
        updatedAt: timestamp,
      });
      scheduleActivityPrune(now);
      return null;
    case "proceed":
    case "session-initiate":
    case "session-accept":
      const eventProvidedJoin = "join" in envelope.event ? envelope.event.join : undefined;
      const cachedJoin = eventProvidedJoin
        ? null
        : readDmCallJoin({
          selfBareJid: envelope.selfBareJid,
          peerJid,
          sid: envelope.event.sid,
          selfFullJid: eventSelfFullJid(envelope),
          now,
        });
      const media = eventMedia(envelope.event, previousForSid);
      const restoredMedia = media.known || !cachedJoin
        ? media
        : { media: cachedJoin.media, known: true };
      const join = eventJoin(envelope.event, previousForSid) ?? cachedJoin?.join;
      const nextRemoteFullJid = previousForSid?.remoteFullJid ?? remoteFullJid ?? cachedJoin?.remoteFullJid;
      if (eventProvidedJoin && nextRemoteFullJid) {
        const selfFullJid = eventSelfFullJid(envelope, eventProvidedJoin);
        if (selfFullJid && selfFullJid === eventProvidedJoin.identity) {
          rememberDmCallJoin({
            selfBareJid: envelope.selfBareJid,
            entry: {
              peerJid,
              sid: envelope.event.sid,
              selfFullJid,
              remoteFullJid: nextRemoteFullJid,
              media: restoredMedia.media,
              join: eventProvidedJoin,
              updatedAt: timestamp,
            },
            now,
          });
        }
      }
      setActivity(peerJid, envelope.event.sid, join?.identity ?? eventSelfFullJid(envelope), {
        peerJid,
        ...(nextRemoteFullJid
          ? { remoteFullJid: nextRemoteFullJid }
          : {}),
        sid: envelope.event.sid,
        media: restoredMedia.media,
        ...(restoredMedia.known ? {} : { mediaKnown: false }),
        ...(join ? { join } : {}),
        state: "accepted",
        direction: previousForSid?.direction ?? direction,
        updatedAt: timestamp,
      });
      scheduleActivityPrune(now);
      return null;
    case "reject":
    case "retract":
    case "finish":
    case "session-terminate":
      if (isOlderThanTerminal(peerJid, envelope.event.sid, timestamp)) return null;
      const outcomeAnchor = terminalOutcomeAnchor({
        peerJid,
        sid: envelope.event.sid,
        timestamp,
        event: envelope.event,
        previous: previousForSid,
        direction,
        selfJid: envelope.selfFullJid ?? envelope.selfBareJid,
      });
      if (envelope.publishOutcome !== false) {
        publishDmCallOutcomeAnchor(outcomeAnchor);
      }
      recordTerminal(peerJid, envelope.event.sid, timestamp, now);
      forgetDmCallJoin({ selfBareJid: envelope.selfBareJid, peerJid, sid: envelope.event.sid });
      removeActivity(peerJid, envelope.event.sid);
      return outcomeAnchor;
  }
}

export function clearDmCallActivities(): void {
  clearPruneTimer();
  $dmCallActivities.set({});
  dmCallTerminalTimestamps.clear();
}

export function readDmCallActivity(
  peerJid: string,
  now = new Date(),
  selfFullJid?: string | null,
): DmCallActivity | null {
  pruneExpiredDmCallActivities(now);
  return bestActivityForPeer(peerJid, selfFullJid);
}

function readDmCallActivityForSid(
  peerJid: string,
  sid: string | null | undefined,
  now = new Date(),
): DmCallActivity | null {
  pruneExpiredDmCallActivities(now);
  if (!sid) return readDmCallActivity(peerJid, now);
  return findActivityForSid(peerJid, sid)?.[1] ?? null;
}

export function readEndableDmCallActivity(
  peerJid: string,
  selfFullJid: string | null | undefined,
  sid?: string | null,
  now = new Date(),
): DmCallActivity | null {
  pruneExpiredDmCallActivities(now);
  const candidates = sid
    ? [readDmCallActivityForSid(peerJid, sid, now)].filter((activity): activity is DmCallActivity => !!activity)
    : dmCallActivitiesForPeer($dmCallActivities.get(), peerJid);
  return candidates.find((activity) => canEndRecoveredDmCallActivity(activity, selfFullJid)) ?? null;
}

export function useDmCallActivity(
  peerJid: () => string | null | undefined,
): {
  activity: ComputedRef<DmCallActivity | null>;
  hasActivity: ComputedRef<boolean>;
} {
  const activities = useStore($dmCallActivities);
  const activity = computed<DmCallActivity | null>(() => {
    return dmCallActivitiesForPeer(activities.value, peerJid() ?? "")[0] ?? null;
  });
  const hasActivity = computed<boolean>(() => activity.value !== null);
  return { activity, hasActivity };
}
