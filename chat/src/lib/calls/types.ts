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

type CallEventEnvelope = {
  from: string;
  to?: string | null;
  sid: string;
};

export type CallEvent =
  // `from` is the sender's full JID for every inbound event. The
  // only bare-addressed send path is outbound `<propose/>`.
  | (CallEventEnvelope & { kind: "propose"; media: CallMedia })
  | (CallEventEnvelope & { kind: "ringing" })
  | (CallEventEnvelope & { kind: "proceed" })
  | (CallEventEnvelope & { kind: "reject"; reason?: string | null; tieBreak?: boolean })
  | (CallEventEnvelope & { kind: "retract"; reason?: string | null; tieBreak?: boolean })
  | (CallEventEnvelope & { kind: "finish"; reason?: string | null; migratedTo?: string | null })
  | (CallEventEnvelope & {
      kind: "session-initiate";
      media: CallMedia;
      join: LiveKitJoin;
    })
  | (CallEventEnvelope & {
      kind: "session-accept";
      media: CallMedia;
      join: LiveKitJoin;
    })
  | (CallEventEnvelope & { kind: "session-terminate"; reason?: string | null });

/**
 * The chat-side reduced state representing the lifecycle of a
 * single call slot. The current implementation tracks one call at
 * a time; if concurrent calls become a requirement later, switch
 * the store from an atom to a map keyed by `sid`.
 */
export type CallState =
  | { phase: "idle" }
  | { phase: "incoming"; from: string; sid: string; media: CallMedia; accepting?: boolean }
  | { phase: "outgoing"; to: string; sid: string; media: CallMedia; initiator?: string; ringing?: boolean }
  | {
      phase: "muc-pending";
      peer: string;
      sid: string;
      media: CallMedia;
      kind: "muc";
      selfNick: string;
      selfFullJid?: string | null;
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
      selfFullJid?: string | null;
    }
  | { phase: "ended"; sid: string; reason: string | null };
