import type {
  ExtensionAnnotation,
  LinkPreview,
  MarkupSpan,
  MessageReference,
} from "@/lib/chat-ui";
import type { TimestampSource } from "@/lib/timeline-timestamps";
import type { LiveDmMessage, LiveRoomMessage } from "@/lib/xmpp-client";

// Stage-1 shared message shape for the channel/DM merge-core unification.
//
// `MergeableMessage` is the structural intersection of `LiveRoomMessage`
// and `LiveDmMessage` restricted to exactly the fields the five merge
// concerns read: retraction (XEP-0424/0425), correction (XEP-0308),
// reactions (XEP-0444), displayed markers (XEP-0333), and self-echo
// reconciliation (XEP-0359 ids + body fallback). Both live message types
// already satisfy it structurally — no runtime adapters, no renames.
//
// ── Field-name / semantics mapping for stage 2 (recorded, not resolved) ──
//
// Sender identity key:
//   * channel: (`roomJid`, `nick`) → occupant JID `${roomJid}/${nick}`,
//     optionally strengthened by `authorRealJid` (bare-JID compare) —
//     see `mucCorrectionSender` / `isSameMucCorrectionSender`.
//   * dm: `fromJid` (bare-JID compare) — see `isSameDmCorrectionSender`.
//   `fromJid` is required on `LiveDmMessage` but optional on
//   `LiveRoomMessage`, so the shared shape can only carry it as optional;
//   `roomJid` / `authorRealJid` stay channel-only, `peerJid` stays DM-only.
//
// Conversation scope key: channel `roomJid` vs dm `peerJid` — no shared
//   name; stage 2 needs an explicit mapping (e.g. `scopeId`).
//
// Reaction targeting:
//   * channel: `reactionTargetId` (room-assigned XEP-0359 stanza-id) plus
//     `_reactionSenderId` for occupant-keyed sender bookkeeping —
//     channel-only fields.
//   * dm: reactions resolve against `id` / `wireIds`; senders are keyed
//     by nick only.
//
// Moderation (XEP-0425): `moderationTargetId` / `moderatedBy` /
//   `moderationReason` exist only on `LiveRoomMessage`.
//
// Stanza `type`: `"message" | "subject" | "pin-event"` on the room shape
//   vs the literal `"message"` on the DM shape; the channel pipeline
//   classifies on it, the DM pipeline never sees non-message types.
export interface MergeableMessage {
  // ── identity (XEP-0359 stanza/origin ids + MAM archive id) ──
  id: string;
  wireIds?: string[];
  archiveId?: string;
  stanzaId?: string;
  stanzaIdBy?: string;
  /** XEP-0308 correction target id advertised by the sender. */
  correctionTargetId?: string;
  /** XEP-0461 §3.2 replyable id. */
  replyableId?: string;

  // ── sender key (see mapping note above) ──
  nick: string;
  fromJid?: string;

  // ── body + XEP-0308 correction payload ──
  body: string;
  replacesId?: string;
  markup?: MarkupSpan[];
  references?: MessageReference[];
  linkPreviews?: LinkPreview[];
  extensionAnnotations?: ExtensionAnnotation[];
  extensionBodyFallback?: boolean;

  // ── XEP-0424 retraction ──
  retractsId?: string;
  retractionId?: string;
  isRetracted?: boolean;

  // ── XEP-0444 reaction bookkeeping (MAM replay) ──
  _reactionTarget?: string;
  _reactionEmojis?: string[];

  // ── XEP-0333 displayed markers ──
  displayedMarkerRequested?: boolean;

  // ── timestamps (merge picks the higher-authority source) ──
  createdAt: string;
  createdAtSource: TimestampSource;
}

// Compile-time conformance: both live message types must satisfy the
// shared shape structurally. Pure type-level — erased at runtime.
type Satisfies<T extends Shape, Shape> = T;
type _LiveRoomMessageIsMergeable = Satisfies<LiveRoomMessage, MergeableMessage>;
type _LiveDmMessageIsMergeable = Satisfies<LiveDmMessage, MergeableMessage>;
