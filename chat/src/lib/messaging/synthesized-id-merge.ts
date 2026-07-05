import type { TimelineMessage } from "@/lib/chat-ui";
import { mergeMessageIds } from "@/lib/message-ids";
import { barePeerJid } from "@/lib/xmpp-client";

// #1182: a live stanza with no wire identity (no client id, no XEP-0359
// stanza-id/origin-id) is keyed on a fabricated UUID (`synthesizedId`).
// Its MAM copy carries the real archive UID, so id/alias lookups can
// never match the two — content-based reconciliation is the only bridge.
// The fabricated UUID never went on the wire, so nothing else (reactions,
// retractions, corrections) can reference it; adopting the archive
// identity is always safe.

/**
 * Bound on live-receipt-time vs archive-stamp divergence. Generous enough
 * for clock skew, tight enough that a user repeating the same message
 * later doesn't collapse into the earlier row.
 */
const SYNTHESIZED_MATCH_WINDOW_MS = 5 * 60_000;

/**
 * How far the real (archive) stamp may *lead* the synthesized receipt
 * stamp. The archive records the same message the client already
 * received, so it can only lead by clock skew — a same-content row
 * stamped later than this is a distinct message sent while we were
 * offline and must never be swallowed.
 */
const REAL_STAMP_AHEAD_TOLERANCE_MS = 90_000;

/**
 * Same-author check that works for both pipelines: MUC rows compare the
 * room-occupant JID (the MAM copy may add a real JID the live copy never
 * saw); DM rows compare the bare sender JID.
 */
function sameAuthor(a: TimelineMessage, b: TimelineMessage): boolean {
  if (a.authorOccupantJid || b.authorOccupantJid) {
    return a.authorOccupantJid === b.authorOccupantJid;
  }
  return a.author === b.author
    && barePeerJid(a.authorJid ?? "") === barePeerJid(b.authorJid ?? "");
}

/**
 * Asymmetric stamp check: the real side may trail the synthesized
 * receipt by the full window (slow delivery, skew) but only lead it by
 * the skew tolerance (see `REAL_STAMP_AHEAD_TOLERANCE_MS`).
 */
function stampsReconcilable(synthesized: TimelineMessage, real: TimelineMessage): boolean {
  const synthesizedStamp = Date.parse(synthesized.createdAt);
  const realStamp = Date.parse(real.createdAt);
  if (!Number.isFinite(synthesizedStamp) || !Number.isFinite(realStamp)) return false;
  return realStamp - synthesizedStamp <= REAL_STAMP_AHEAD_TOLERANCE_MS
    && synthesizedStamp - realStamp <= SYNTHESIZED_MATCH_WINDOW_MS;
}

/**
 * The comparable content of a row: body AND attachment URLs together, in
 * a fixed composite shape so a caption shared by two different file
 * shares can't match, and a body that spells out the file section can't
 * collide with a genuine file-only key. Empty for rows with neither
 * (e.g. retraction tombstones), which therefore never content-match.
 */
function contentKey(message: TimelineMessage): string {
  const urls = (message.sharedFiles ?? []).map((file) => file.url);
  if (!message.body && !urls.length) return "";
  // Newline-delimited sections: URLs cannot contain raw newlines, so a
  // body can never forge the files-section boundary.
  return `body:${message.body}\nfiles:${urls.join("\n")}`;
}

/**
 * Finds the row an identity-less arrival reconciles into: same author,
 * same content, timestamps within the match window — and exactly one side
 * synthesized, so two genuinely distinct id-less messages with equal
 * content can never collapse into each other.
 */
export function findSynthesizedIdMergeTarget(
  candidates: readonly TimelineMessage[],
  incoming: TimelineMessage,
): TimelineMessage | undefined {
  const incomingKey = contentKey(incoming);
  if (!incomingKey) return undefined;
  return candidates.find((existing) =>
    !existing.synthesizedId !== !incoming.synthesizedId
    && contentKey(existing) === incomingKey
    && sameAuthor(existing, incoming)
    && (incoming.synthesizedId
      ? stampsReconcilable(incoming, existing)
      : stampsReconcilable(existing, incoming))
  );
}

const ADOPTED_IDENTITY_FIELDS = [
  "replyableId",
  "stanzaId",
  "stanzaIdBy",
  "reactionTargetId",
  "correctionTargetId",
] as const;

/**
 * Replaces a reconciled row's fabricated identity with the archive copy's
 * real one: the archive primary id wins (the UUID is demoted to a wire
 * alias), the archive-derived id fields the id-less live stanza could
 * never provide are adopted, and the `synthesizedId` flag is cleared.
 */
export function adoptArchiveIdentity(
  row: TimelineMessage,
  archiveRow: TimelineMessage,
): TimelineMessage {
  const adopted = mergeMessageIds(row, archiveRow.id, archiveRow.wireIds);
  delete adopted.synthesizedId;
  for (const field of ADOPTED_IDENTITY_FIELDS) {
    const value = archiveRow[field];
    if (adopted[field] === undefined && value !== undefined) adopted[field] = value;
  }
  return adopted;
}
