/**
 * Typed inbound call event surfaced by the WASM client's `on_call`
 * callback. Mirrors `WaddleCallEvent` produced by
 * `server/crates/waddle-xmpp-client-wasm/src/types.rs`.
 *
 * The `kind` discriminator drives a switch on the chat side; see
 * `CallEvent` for the exhaustive variant list and which fields are
 * populated per variant.
 */
export type CallMedia = { audio: boolean; video: boolean };

export type LiveKitJoin = {
  url: string;
  room: string;
  identity: string;
  token: string;
};

export type CallEvent =
  // `from` is the sender's full JID for every inbound event. The
  // only bare-addressed send path is outbound `<propose/>`.
  | { kind: "propose"; from: string; sid: string; media: CallMedia }
  | { kind: "proceed"; from: string; sid: string }
  | { kind: "reject"; from: string; sid: string; reason?: string | null; tieBreak?: boolean }
  | { kind: "retract"; from: string; sid: string; reason?: string | null; tieBreak?: boolean }
  | { kind: "finish"; from: string; sid: string; reason?: string | null; migratedTo?: string | null }
  | {
      kind: "session-initiate";
      from: string;
      sid: string;
      media: CallMedia;
      join: LiveKitJoin;
    }
  | {
      kind: "session-accept";
      from: string;
      sid: string;
      media: CallMedia;
      join: LiveKitJoin;
    }
  | { kind: "session-terminate"; from: string; sid: string; reason: string | null };

/**
 * The chat-side reduced state representing the lifecycle of a
 * single call slot. The current implementation tracks one call at
 * a time; if concurrent calls become a requirement later, switch
 * the store from an atom to a map keyed by `sid`.
 */
export type CallState =
  | { phase: "idle" }
  | { phase: "incoming"; from: string; sid: string; media: CallMedia }
  | { phase: "outgoing"; to: string; sid: string; media: CallMedia; initiator?: string }
  | {
      phase: "muc-pending";
      peer: string;
      sid: string;
      media: CallMedia;
      kind: "muc";
      selfNick: string;
      attemptId: string;
      activePresencePublished?: boolean;
    }
  | {
      phase: "active";
      /** 1:1 calls track the peer's full JID here; group calls
       *  store the room's bare JID. Distinguish via `kind`. */
      peer: string;
      sid: string;
      media: CallMedia;
      join: LiveKitJoin;
      kind: "dm" | "muc";
      initiator?: string;
      /** MUC group calls only: the nick we used to join the room.
       *  Stored so `tearDownActiveCall` can emit the matching
       *  Muji-clearing presence update for XEP-0272 §Leaving. */
      selfNick?: string;
    }
  | { phase: "ended"; sid: string; reason: string | null };
