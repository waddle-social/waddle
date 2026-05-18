/** TypeScript interfaces for Rust/WASM callback payload shapes.
 * All fields are snake_case (serde serialization convention).
 */

export interface WasmMarkupSpan {
  span_type: string;
  start: number;
  end: number;
  uri?: string;
}

export interface WasmEncryptedFileHash {
  algo: string;
  value_b64: string;
}

export interface WasmEncryptedFile {
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

export interface WasmStanzaId {
  id: string;
  by: string;
}

export interface WasmExtensionEnvelope {
  version: number;
  enrichments: WasmExtensionEnrichment[];
}

export interface WasmExtensionEnrichment {
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

export interface WasmExtensionSource {
  stanza_id: string;
  body_start?: number;
  body_end?: number;
}

export interface WasmExtensionLaunch {
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

export interface WasmExtensionLaunchContext {
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
  displayed_marker_id?: string;
  reaction_target_id?: string;
  reaction_emojis: string[];
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
  shared_files: WasmSharedFile[];
  extension_envelope?: WasmExtensionEnvelope;
  extension_body_fallback?: boolean;
  /** urn:waddle:pin:0 pin/unpin event surfaced by the room (#414). */
  pin_event?: WasmPinEvent;
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
export interface WasmPinPreview {
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

export interface WasmPresenceHat {
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
  vcard_avatar?: string;
}

export interface WasmArchivedMessage extends WasmMessage {
  mam_id: string;
  query_id?: string;
  author_real_jid?: string;
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
  total_unread: number;
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
}

/** XEP-0490 §3 displayed entry surfaced from the wasm boundary. */
export interface WasmMdsDisplayedEntry {
  chat_id: string;
  stanza_id: string;
  stanza_id_by: string;
}

/** Options for send_groupchat_message / send_chat_message. */
export interface WasmSendOptions {
  stanza_id?: string;
  subject?: string;
  reply?: { author_jid: string; message_id: string };
  fallback?: { start: number; end: number };
  thread?: { id: string; parent?: string };
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

export interface WasmAdminChannelListEntry {
  channel_jid: string;
  name: string;
  topic?: string | null;
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
  is_public: boolean;
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
