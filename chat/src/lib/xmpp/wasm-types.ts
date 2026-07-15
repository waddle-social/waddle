import type { CallEvent } from "@/lib/calls/types";

export type WasmStreamManagementTelemetry =
  | { kind: "ack-request"; attempt: number; unacked: number }
  | { kind: "ack-observed"; progressed: boolean; latencyMs?: number | null; unacked: number }
  | { kind: "ack-request-timeout"; unacked: number }
  | { kind: "ack-progress-stalled"; unacked: number; elapsedMs: number };

/** TypeScript interfaces for Rust/WASM callback payload shapes.
 * All fields are snake_case (serde serialization convention).
 */

interface WasmMarkupSpan {
  span_type: string;
  start: number;
  end: number;
  uri?: string;
}

interface WasmEncryptedFileHash {
  algo: string;
  value_b64: string;
}

interface WasmEncryptedFile {
  cipher: string;
  key_b64: string;
  iv_b64: string;
  hashes: WasmEncryptedFileHash[];
  sources: string[];
}

export interface WasmSharedFile {
  url: string;
  name?: string;
  media_type?: string;
  size?: number;
  width?: number;
  height?: number;
  disposition: string;
  encrypted?: WasmEncryptedFile;
}

export interface WasmReference {
  ref_type: string;
  uri: string;
  begin: number;
  end: number;
  anchor?: string;
}

interface WasmStanzaId {
  id: string;
  by: string;
}

export interface WasmCallThreadAnchor {
  kind: "muc" | "dm";
  sid: string;
  media: Array<"audio" | "video">;
  initiator: string;
  started: string;
}

interface WasmCallThreadEnded {
  anchor_id: string;
  ended: string;
  duration: string;
}

export interface WasmExtensionEnvelope {
  version: number;
  enrichments: WasmExtensionEnrichment[];
}

interface WasmExtensionEnrichment {
  id: string;
  plugin: string;
  capability: string;
  payload_namespace: string;
  created: string;
  source?: WasmExtensionSource;
  title?: string;
  summary?: string;
  payloads: WasmExtensionPayloadElement[];
  launches: WasmExtensionLaunch[];
}

interface WasmExtensionSource {
  stanza_id: string;
  body_start?: number;
  body_end?: number;
}

interface WasmExtensionLaunch {
  id: string;
  plugin: string;
  action: string;
  command_node: string;
  label: string;
  context: WasmExtensionLaunchContext;
  payloads: WasmExtensionPayloadElement[];
  expires_at?: string;
  token?: string;
}

interface WasmExtensionLaunchContext {
  waddle_id: string;
  room?: string;
  source_stanza_id?: string;
}

export interface WasmExtensionPayloadElement {
  namespace: string;
  name: string;
  attributes: Array<{ name: string; value: string }>;
  text?: string;
  children: WasmExtensionPayloadElement[];
}

export interface WasmMessage {
  id?: string;
  from?: string;
  to?: string;
  body?: string;
  subject?: string;
  message_type: string;
  timestamp?: string;
  stanza_id?: string;
  stanza_id_by?: string;
  stanza_ids?: WasmStanzaId[];
  origin_id?: string;
  replaces_id?: string;
  retracts_id?: string;
  retraction_id?: string;
  is_retracted?: boolean;
  moderation_target_id?: string;
  moderated_by?: string;
  moderation_reason?: string;
  chat_state?: string;
  displayed_marker_requested?: boolean;
  displayed_marker_id?: string;
  reaction_target_id?: string;
  reaction_emojis: string[];
  in_call?: { kind: "reaction"; sid: string; emoji: string };
  is_muc: boolean;
  thread?: string;
  parent_thread_id?: string;
  reply_to_id?: string;
  reply_to_sender?: string;
  reply_fallback_start?: number;
  reply_fallback_end?: number;
  markup_spans: WasmMarkupSpan[];
  broadcast_mention?: string;
  mention_uris: string[];
  references: WasmReference[];
  forum_post_kind?: string;
  forum_title?: string;
  forum_thread_title?: string;
  is_sticker: boolean;
  call_thread?: WasmCallThreadAnchor;
  call_thread_ended?: WasmCallThreadEnded;
  shared_files: WasmSharedFile[];
  link_previews: WasmLinkPreview[];
  call_event?: CallEvent;
  extension_envelope?: WasmExtensionEnvelope;
  extension_body_fallback?: boolean;
  /** urn:waddle:pin:0 pin/unpin event surfaced by the room (#414). */
  pin_event?: WasmPinEvent;
  inbox_push?: WasmInboxConversation;
  /**
   * XEP-0280: present when the WASM core unwrapped this message from a
   * verified carbon envelope (#1243). Exactly one flag is true. The §11
   * own-bare-JID forgery check already happened in the core — a message
   * carrying this marker is always a trustworthy carbon copy.
   */
  carbon?: { sent: boolean; received: boolean };
}

export interface WasmLinkPreview {
  original_url: string;
  normalized_url?: string | null;
  title?: string | null;
  description?: string | null;
  image?: WasmLinkPreviewImage | null;
  video?: WasmLinkPreviewVideo | null;
  player_embed?: WasmLinkPreviewPlayer | null;
  remote_media_unavailable?: boolean;
}

interface WasmLinkPreviewVideo {
  url: string;
  media_type: string;
}

interface WasmLinkPreviewImage {
  url: string;
  media_type: string;
  width?: number | null;
  height?: number | null;
  alt?: string | null;
}

interface WasmLinkPreviewPlayer {
  url: string;
  width?: number | null;
  height?: number | null;
}

/** Pin/unpin event from a room system message (#414). */
export interface WasmPinEvent {
  action: "pinned" | "unpinned";
  target_stanza_id: string;
  by: string;
  /** "retracted" when the unpin was triggered by an XEP-0424 cascade. */
  reason?: string;
  preview?: WasmPinPreview;
}

/** Frozen preview snapshot of a pinned message (#414). */
interface WasmPinPreview {
  author_jid: string;
  author_nick?: string;
  text: string;
  /** rfc3339. */
  message_timestamp: string;
}

/** One pinned-message entry returned by `fetchRoomPins` (#414). */
export interface WasmPinEntry {
  target_stanza_id: string;
  pinner_jid: string;
  /** rfc3339. */
  pinned_at: string;
  preview: WasmPinPreview;
}

interface WasmPresenceHat {
  uri: string;
  title: string;
}

export interface WasmPresence {
  from?: string;
  to?: string;
  presence_type: string;
  show?: string;
  status?: string;
  hats: WasmPresenceHat[];
  muc_affiliation?: string;
  muc_role?: string;
  muc_jid?: string;
  /**
   * XEP-0045 MUC `<status code='…'/>` markers as raw numbers (e.g.
   * `110` self-presence, `100` non-anonymous). Absent when the
   * presence carries no status codes.
   */
  muc_status_codes?: number[];
  vcard_avatar?: string;
  muji?: WasmMujiPresence;
  /**
   * Waddle raised-hand call presence state (#1029). `true` when the
   * occupant's presence carries
   * `<in-call xmlns='urn:waddle:in-call:0'><hand-raised/></in-call>`
   * alongside `<muji/>`. Absent/false means the hand is lowered.
   */
  hand_raised?: boolean;
  /**
   * Waddle mute call presence state (#1030). `true` when the occupant's
   * presence carries a `<muted/>` marker on the same `<in-call>` carrier.
   * Absent/false means the microphone is unmuted. Drives the
   * authoritative remote-mute indicator on tiles and in the roster.
   */
  muted?: boolean;
  /**
   * XEP-0319 Last User Interaction — the raw `since` xs:dateTime from the
   * contact's `<idle xmlns='urn:xmpp:idle:1'/>`, or absent when the presence
   * carries none. The chat side renders the idle age on an away presence.
   */
  idle_since?: string;
  /**
   * RFC 6120 §8.3 defined condition of an error presence (e.g. a rejected
   * XEP-0045 room join). Absent for non-error presences.
   */
  error_condition?: string;
  /** RFC 6120 §8.3.2 `type` attribute of the `<error/>` element. */
  error_type?: string;
  /** Server-provided `<text/>` of the error, absent when none. */
  error_text?: string;
}

/**
 * XEP-0272 Muji presence extension surfaced from the WASM bindings.
 * `active` is true when the presence advertised at least one
 * `<content/>` child; `audio` / `video` mirror the advertised RTP
 * media content; `preparing` is true when a `<preparing/>` sentinel
 * was present (XEP-0272 §Joining two-phase flow).
 * Absence of the field (`muji` undefined) means the occupant is NOT
 * in the call — XEP-0272 §Leaving "absence is the leave marker."
 */
interface WasmMujiPresence {
  preparing: boolean;
  active: boolean;
  audio: boolean;
  video: boolean;
}

export interface WasmArchivedMessage extends WasmMessage {
  mam_id: string;
  query_id?: string;
  author_real_jid?: string;
  call_event?: CallEvent;
}

export interface WasmMamPage {
  messages: WasmArchivedMessage[];
  first_id?: string;
  last_id?: string;
  is_complete: boolean;
}

export interface WasmInboxConversation {
  partner: string;
  kind: string;
  last_stanza_id: string;
  last_updated: number;
  unread: number;
  preview?: string;
  thread?: string;
  thread_title?: string;
  reply_count?: number;
  author?: string;
}

export interface WasmInboxResult {
  /** XEP-0430 `<fin all-unread/>` — total unread across the inbox. */
  total_unread: number;
  /** XEP-0430 `<fin total/>` — number of conversations in this page. */
  total: number;
  /** XEP-0430 `<fin unread/>` — number of conversations with unread > 0. */
  unread_conversations: number;
  conversations: WasmInboxConversation[];
}

export interface WasmRosterContact {
  jid: string;
  name?: string;
  subscription?: string;
  groups: string[];
}

export interface WasmAvatar {
  jid: string;
  id: string;
  mime_type: string;
  data?: Uint8Array;
  url?: string;
}

export interface WasmUploadSlot {
  put_url: string;
  get_url: string;
  put_headers: Array<{ name: string; value: string }>;
}

export interface WasmServerVersion {
  name?: string;
  version?: string;
  os?: string;
}

export interface WasmRoomMember {
  jid: string;
  affiliation: string;
}

export interface WasmUserSearchResult {
  jid: string;
  username?: string;
  display_name?: string;
  nick?: string;
  name?: string;
}

export interface WasmPepProfile {
  mood?: { kind: string; text?: string } | null;
  activity?: { general: string; specific?: string; text?: string } | null;
  tune?: {
    artist?: string;
    title?: string;
    source?: string;
    length?: number;
    rating?: number;
    track?: string;
    uri?: string;
  } | null;
}

/**
 * XEP-0292 vCard4 payload exchanged over the wasm boundary. Mirrors the Rust
 * `WaddleVCard4` struct: every property is optional, and the `fn` field uses
 * the XEP-0292 spelling rather than `fullName` so the serde rename on the Rust
 * side stays a 1:1 wire mapping.
 */
export interface WasmVCard4 {
  fn?: string;
  nickname?: string;
  pronouns?: string;
  note?: string;
  url?: string;
  photo_uri?: string;
}

/** Call-thread anchor summary on a thread entry. */
interface WasmThreadCallAnchor {
  kind: "muc" | "dm";
  media: Array<"audio" | "video">;
}

/** Ended-call summary on a thread entry. */
interface WasmThreadCallEnded {
  /** RFC 3339 timestamp. */
  ended: string;
  /** ISO-8601 duration, e.g. "PT5M". */
  duration: string;
}

/** One entry in a urn:waddle:threads:0 response. */
export interface WasmThreadEntry {
  channel: string;
  thread_id: string;
  last_stanza_id: string;
  /** RFC 3339 timestamp. */
  last_activity: string;
  unread: number;
  reply_count: number;
  has_unread: boolean;
  root_author?: string;
  preview?: string;
  thread_title?: string;
  /** Present when this thread anchors a call. */
  callThread?: WasmThreadCallAnchor;
  /** Present when the anchored call has ended. */
  callThreadEnded?: WasmThreadCallEnded;
}

/** Paged response to a urn:waddle:threads:0 query. */
export interface WasmThreadsPage {
  total: number;
  unread_threads: number;
  entries: WasmThreadEntry[];
  next_cursor?: string;
}

/** Options bag for fetchThreads. */
export interface WasmFetchThreadsOptions {
  page_size?: number;
  after_cursor?: string;
  status?: "all" | "unread" | "following";
  active_since?: string;
  channel?: string;
  search?: string;
  sort?: "recent" | "unread" | "replies";
}

/** XEP-0490 §3 displayed entry surfaced from the wasm boundary. */
export interface WasmMdsDisplayedEntry {
  chat_id: string;
  stanza_id: string;
  stanza_id_by: string;
}

export interface WasmPubsubEvent {
  from?: string;
  node: string;
  items: WasmPubsubEventItem[];
}

interface WasmPubsubEventItem {
  id?: string;
  retracted?: boolean;
  payload: WasmPubsubEventPayload;
}

type WasmPubsubEventPayload =
  | {
      kind: "attachmentSummary";
      reactions: Array<{ emoji: string; count: number; reactors: string[] }>;
      noticedCount?: number;
    }
  | { kind: "opaque"; xml: string }
  | { kind: "empty" };

/** Options for send_groupchat_message / send_chat_message. */
export interface WasmSendOptions {
  stanza_id?: string;
  subject?: string;
  reply?: { author_jid: string; message_id: string };
  fallback?: { start: number; end: number };
  thread?: { id: string; parent?: string };
  link_preview_token?: string;
  request_displayed_marker?: boolean;
  /** XEP-0045 §7.5 MUC private message marker. */
  muc_pm?: boolean;
  shared_files?: Array<{
    url: string;
    name?: string;
    media_type?: string;
    size?: number;
    width?: number;
    height?: number;
    disposition: string;
    encrypted?: WasmEncryptedFile;
  }>;
  markup_spans?: WasmMarkupSpan[];
  references?: WasmReference[];
}

export type WasmSendMessageOutcome =
  | { kind: "sent"; stanza_id?: string; stanzaId?: string }
  | { kind: "not-connected" }
  | { kind: "invalid-recipient" }
  | { kind: "invalid-options" }
  | { kind: "stanza-error" }
  | { kind: "transport-error" }
  | { kind: "error" };

// ─── Admin V2 — Spaces ────────────────────────────────────────────────
//
// TypeScript mirrors of the typed Result structs in
// `server/crates/waddle-xmpp-client-wasm/src/client_admin_v2.rs`.
// snake_case matches the serde wire form. The Args structs are not
// re-exported here because `BrowserXmppClient` wrappers expose
// camelCase parameter objects and translate to snake_case before
// invoking wasm.

export interface WasmAdminSpaceListEntry {
  space_jid: string;
  space_node: string;
  name: string;
  description?: string | null;
  icon_url?: string | null;
  channel_count: number;
  member_count: number;
}

export interface WasmAdminSpacesListResult {
  entries: WasmAdminSpaceListEntry[];
  next_cursor?: string | null;
}

export interface WasmAdminSpaceRef {
  space_jid: string;
  space_node: string;
  name: string;
  description?: string | null;
  icon_url?: string | null;
}

export interface WasmAdminSpaceMemberEntry {
  jid: string;
  /** `owner` | `admin` | `member` | `none`. */
  role: string;
}

export interface WasmAdminSpacesMembersResult {
  entries: WasmAdminSpaceMemberEntry[];
  next_cursor?: string | null;
}

export interface WasmAdminSpacesSetRoleResult {
  member_jid: string;
  role: string;
}

// ─── Admin V2 — Channels ──────────────────────────────────────────────

export type WasmAdminChannelType = "text" | "announcement" | "forum" | "group-dm";

export interface WasmAdminChannelListEntry {
  channel_jid: string;
  name: string;
  topic?: string | null;
  channel_type: WasmAdminChannelType;
  is_public: boolean;
  members_only: boolean;
  occupant_count: number;
  owner_count: number;
  admin_count: number;
  member_count: number;
  outcast_count: number;
}

export interface WasmAdminChannelsListResult {
  entries: WasmAdminChannelListEntry[];
  next_cursor?: string | null;
}

export interface WasmAdminChannelRef {
  channel_jid: string;
  name: string;
  topic?: string | null;
  channel_type: WasmAdminChannelType;
  is_public: boolean;
  members_only: boolean;
}

export interface WasmAdminChannelOccupantEntry {
  nick: string;
  real_jid: string;
  /** `moderator` | `participant` | `visitor` | `none`. */
  role: string;
  /** `owner` | `admin` | `member` | `none` | `outcast`. */
  affiliation: string;
}

export interface WasmAdminChannelsOccupantsResult {
  entries: WasmAdminChannelOccupantEntry[];
  next_cursor?: string | null;
}

export interface WasmAdminChannelAffiliationEntry {
  jid: string;
  affiliation: string;
  reason?: string | null;
}

export interface WasmAdminChannelsAffiliationsResult {
  entries: WasmAdminChannelAffiliationEntry[];
  next_cursor?: string | null;
}

export interface WasmAdminChannelsSetAffiliationResult {
  member_jid: string;
  affiliation: string;
}

export interface WasmAdminChannelsKickResult {
  occupant_jid: string;
}
