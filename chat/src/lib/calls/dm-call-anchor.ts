import { atom } from "nanostores";
import type { CallThreadAnchor, TimelineMessage } from "@/lib/chat-ui";
import { barePeerJid, jidLocalpart } from "../xmpp/jid";
import type { CallMedia, CallState } from "./types";

/**
 * A 1:1 (DM) call that has just become active and should surface a "started a
 * call" anchor card in the peer conversation's live timeline — the DM analogue
 * of the server-broadcast MUC anchor.
 *
 * Unlike MUC (where the server broadcasts a live groupchat anchor), the DM
 * anchor is only enriched onto the archived XEP-0353 `<proceed/>` row, so it
 * never reaches the live timeline. This carries the data needed to synthesize
 * an equivalent in-memory card that the later MAM-replayed archive row dedups
 * against by call sid.
 */
export type DmCallStartedAnchor = {
  peerBareJid: string;
  sid: string;
  media: CallMedia;
  /** Bare or full JID of the call initiator (the server invariant author). */
  initiator: string;
  /** ISO-8601 start time. */
  started: string;
};

export type DmCallOutcome = "declined" | "missed" | "no-answer" | "ended";

export type DmCallOutcomeAnchor = {
  peerBareJid: string;
  sid: string;
  media: CallMedia;
  outcome: DmCallOutcome;
  /** Bare or full JID of the call initiator when known. */
  initiator?: string;
  /** ISO-8601 terminal timestamp. */
  ended: string;
};

/**
 * Deterministic timeline id for a DM call-thread anchor, shared by the
 * synthesized live card and the MAM-replayed archive row (via a wire-id alias)
 * so the two collapse to a single card instead of duplicating.
 */
export function dmCallAnchorId(sid: string): string {
  return `dmcall:${sid}`;
}

export function dmCallOutcomeAnchorId(sid: string, outcome: DmCallOutcome): string {
  return `dmcall:${sid}:${outcome}`;
}

function callThreadMedia(media: CallMedia): ("audio" | "video")[] {
  const kinds: ("audio" | "video")[] = [];
  if (media.audio) kinds.push("audio");
  if (media.video) kinds.push("video");
  // A call always has at least audio; default so the card never renders empty.
  if (kinds.length === 0) kinds.push("audio");
  return kinds;
}

/**
 * Build the in-memory "started a call" anchor card for a live DM call. The
 * card roots its own call thread (`threadId == sid`, matching the server's
 * `thread_id == key.sid.0` invariant) and uses the lowest-authority
 * `fallback` timestamp so the MAM archive stamp wins on merge.
 */
export function buildDmCallStartedAnchor(
  anchor: DmCallStartedAnchor,
  selfJid: string,
): TimelineMessage {
  const initiatorBare = barePeerJid(anchor.initiator).toLowerCase();
  const isSelf = !!initiatorBare && initiatorBare === barePeerJid(selfJid).toLowerCase();
  const callThread: CallThreadAnchor = {
    kind: "dm",
    sid: anchor.sid,
    media: callThreadMedia(anchor.media),
    initiator: anchor.initiator,
    started: anchor.started,
  };
  return {
    id: dmCallAnchorId(anchor.sid),
    author: jidLocalpart(initiatorBare),
    authorJid: anchor.initiator,
    body: "",
    createdAt: anchor.started,
    createdAtSource: "fallback",
    isSelf,
    threadId: anchor.sid,
    callThread,
  };
}

export function buildDmCallOutcomeAnchor(
  anchor: DmCallOutcomeAnchor,
  selfJid: string,
): TimelineMessage {
  const selfBare = barePeerJid(selfJid).toLowerCase();
  const initiator = anchor.initiator
    ? barePeerJid(anchor.initiator).toLowerCase()
    : anchor.outcome === "no-answer"
      ? selfBare
      : "";
  const isSelfInitiated = !!initiator && initiator === selfBare;
  return {
    id: dmCallOutcomeAnchorId(anchor.sid, anchor.outcome),
    author: isSelfInitiated ? jidLocalpart(selfBare) : jidLocalpart(anchor.peerBareJid),
    authorJid: isSelfInitiated ? selfJid : anchor.peerBareJid,
    body: "",
    createdAt: anchor.ended,
    createdAtSource: "fallback",
    isSelf: isSelfInitiated,
    threadId: anchor.sid,
    callThread: {
      kind: "dm",
      sid: anchor.sid,
      media: callThreadMedia(anchor.media),
      ...(anchor.initiator ? { initiator: anchor.initiator } : {}),
      ended: anchor.ended,
      outcome: anchor.outcome,
    },
  };
}

/**
 * Producer guard: derive the anchor to publish from a call-state transition,
 * or `null` when none should be published. Publishes only on the transition
 * INTO an active 1:1 call — not media renegotiations that keep the same active
 * call, and not when the initiator is unknown (the card falls back to MAM
 * replay). Pure so the guard logic is unit-testable without the global store.
 */
export function dmCallStartedAnchorFromTransition(
  before: CallState,
  next: CallState,
  startedIso: string,
): DmCallStartedAnchor | null {
  if (next.phase !== "active" || next.kind !== "dm") return null;
  if (before.phase === "active" && before.kind === "dm" && before.sid === next.sid) return null;
  if (!next.initiator) return null;
  return {
    peerBareJid: barePeerJid(next.peer),
    sid: next.sid,
    media: next.media,
    initiator: next.initiator,
    started: startedIso,
  };
}

/**
 * Consumer guard: build the timeline card for `anchor` only when it belongs to
 * the open conversation, isolating it from other DMs (a call with Bob must not
 * inject a card into Carol's open timeline). Returns `null` otherwise.
 */
export function resolveDmCallAnchorInjection(
  anchor: DmCallStartedAnchor,
  activePeerJid: string | null,
  selfJid: string | null,
): TimelineMessage | null {
  if (!selfJid || !activePeerJid) return null;
  // JID node/domain are case-insensitive (RFC 7622), so normalize before
  // matching the anchor's peer against the open conversation.
  if (
    barePeerJid(anchor.peerBareJid).toLowerCase()
    !== barePeerJid(activePeerJid).toLowerCase()
  ) {
    return null;
  }
  return buildDmCallStartedAnchor(anchor, selfJid);
}

export function resolveDmCallOutcomeInjection(
  anchor: DmCallOutcomeAnchor,
  activePeerJid: string | null,
  selfJid: string | null,
): TimelineMessage | null {
  if (!selfJid || !activePeerJid) return null;
  if (
    barePeerJid(anchor.peerBareJid).toLowerCase()
    !== barePeerJid(activePeerJid).toLowerCase()
  ) {
    return null;
  }
  return buildDmCallOutcomeAnchor(anchor, selfJid);
}

/**
 * Bridge from the call layer (`applyCallEvent`) to the DM messaging
 * composable, which appends the synthesized anchor to the open conversation.
 * A plain atom (last-writer signal) keeps the call store decoupled from the
 * timeline/session — the composable derives `isSelf` from its own session.
 */
export const $dmCallStartedAnchor = atom<DmCallStartedAnchor | null>(null);
export const $dmCallOutcomeAnchor = atom<DmCallOutcomeAnchor | null>(null);

export function publishDmCallStartedAnchor(anchor: DmCallStartedAnchor): void {
  $dmCallStartedAnchor.set(anchor);
}

export function publishDmCallOutcomeAnchor(anchor: DmCallOutcomeAnchor): void {
  $dmCallOutcomeAnchor.set(anchor);
}
