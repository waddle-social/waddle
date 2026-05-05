/** TypeScript interfaces for Rust/WASM callback payload shapes.
 * All fields are snake_case (serde serialization convention).
 */

export interface WasmMarkupSpan {
  span_type: string;
  start: number;
  end: number;
  uri?: string;
}

export interface WasmSharedFile {
  url: string;
  name?: string;
  media_type?: string;
  size?: number;
  width?: number;
  height?: number;
  disposition: string;
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
  origin_id?: string;
  replaces_id?: string;
  retracts_id?: string;
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
  forum_post_kind?: string;
  forum_title?: string;
  forum_thread_title?: string;
  is_sticker: boolean;
  shared_files: WasmSharedFile[];
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

interface WasmInboxResult {
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
  }>;
  markup_spans?: WasmMarkupSpan[];
  references?: Array<{ ref_type: string; uri: string; begin: number; end: number }>;
}
