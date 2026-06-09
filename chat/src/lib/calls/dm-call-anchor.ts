import { atom } from "nanostores";
import type { CallThreadAnchor, TimelineMessage } from "@/lib/chat-ui";
import { barePeerJid } from "../xmpp/jid";
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

/**
 * Deterministic timeline id for a DM call-thread anchor, shared by the
 * synthesized live card and the MAM-replayed archive row (via a wire-id alias)
 * so the two collapse to a single card instead of duplicating.
 */
export function dmCallAnchorId(sid: string): string {
  return `dmcall:${sid}`;
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
    author: initiatorBare.split("@")[0] ?? "unknown",
    authorJid: anchor.initiator,
    body: "",
    createdAt: anchor.started,
    createdAtSource: "fallback",
    isSelf,
    threadId: anchor.sid,
    callThread,
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
  if (barePeerJid(anchor.peerBareJid) !== barePeerJid(activePeerJid)) return null;
  return buildDmCallStartedAnchor(anchor, selfJid);
}

/**
 * Bridge from the call layer (`applyCallEvent`) to the DM messaging
 * composable, which appends the synthesized anchor to the open conversation.
 * A plain atom (last-writer signal) keeps the call store decoupled from the
 * timeline/session — the composable derives `isSelf` from its own session.
 */
export const $dmCallStartedAnchor = atom<DmCallStartedAnchor | null>(null);

export function publishDmCallStartedAnchor(anchor: DmCallStartedAnchor): void {
  $dmCallStartedAnchor.set(anchor);
}
